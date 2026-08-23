use std::time::Instant;

use mural_ipc::{Response, ScaleMode, SetRequest, Transition};

use super::{
    CanvasLayoutSpec, CanvasPreviewPlan, CanvasTile, CanvasTileBuild, CanvasUpload,
    accelerated_canvas_phases, canvas_ready_tile_count,
};
use crate::decode::DecodeJob;
use crate::transitions::{
    QueuedState, QueuedTransition, QueuedWallpaper, Target, TargetPlan, cleanup_canvas_uploads,
};
use crate::{MIN_CANVAS_READY_TILES, MuralApp, validate_image_paths};
use mural_core::wallpaper::PreparedWallpaperChange;

impl MuralApp {
    pub(crate) fn canvas_preview_for_prepared_change(
        &mut self,
        prepared: &PreparedWallpaperChange,
        transition: Transition,
    ) -> Result<Option<CanvasPreviewPlan>, String> {
        let Transition::Canvas {
            pan_axis,
            overview_scale,
            tile_count,
            ..
        } = transition
        else {
            return Ok(None);
        };
        let current = prepared
            .entries
            .iter()
            .filter_map(|entry| {
                self.surfaces
                    .iter()
                    .find(|surface| surface.name == entry.output)
                    .and_then(|surface| surface.current_image.clone())
            })
            .collect::<Vec<_>>();
        let tile_count = self.resolve_canvas_tile_count(tile_count, overview_scale, pan_axis);
        let preview = self
            .wallpaper
            .canvas_preview_window_for_prepared_change(prepared, &current, tile_count)?;

        Ok(Some(CanvasPreviewPlan {
            paths: preview.paths,
            start_index: preview.start_index,
        }))
    }

    pub(crate) fn set_canvas_wallpapers_from_preview(
        &mut self,
        request: &SetRequest,
        preview: &CanvasPreviewPlan,
    ) -> Response {
        if let Err(message) = validate_image_paths(&request.outputs) {
            return Response::Error { message };
        }
        if let Some(response) = self.skip_set_targets_if_all_power_off(request) {
            return response;
        }
        if let Err(response) = self.ensure_set_targets_renderable(request) {
            return response;
        }

        let plan = match self.plan_transition_targets(request, true) {
            Ok(plan) => plan,
            Err(response) => return response,
        };
        self.set_canvas_wallpapers(request, preview, plan)
    }

    pub(crate) fn set_canvas_wallpapers(
        &mut self,
        request: &SetRequest,
        preview: &CanvasPreviewPlan,
        plan: TargetPlan,
    ) -> Response {
        let Transition::Canvas {
            zoom_out_ms,
            pan_ms,
            zoom_in_ms,
            easing,
            mode,
            walk,
            pan_axis,
            overview_scale,
            tile_count,
        } = request.transition
        else {
            return Response::Error {
                message: "set_canvas_wallpapers called with non-canvas transition".to_owned(),
            };
        };
        let layout = CanvasLayoutSpec {
            tile_count,
            mode,
            walk,
            pan_axis,
            overview_scale,
        };
        let uploads = self.prepare_canvas_uploads(plan.starts, preview, layout);

        if uploads
            .iter()
            .any(|upload| upload.ready_tiles < MIN_CANVAS_READY_TILES)
        {
            trace_log!(
                self.trace,
                "set_canvas_wallpapers: not enough ready canvas tiles; falling back to cut"
            );
            cleanup_canvas_uploads(&self.egl, uploads);
            let fallback_plan = match self.plan_transition_targets(request, false) {
                Ok(plan) => plan,
                Err(response) => return response,
            };
            return self.set_cut_wallpapers(request, fallback_plan);
        }

        if let Err(response) = self.schedule_canvas_target_decodes(&uploads) {
            cleanup_canvas_uploads(&self.egl, uploads);
            return response;
        }

        let started = match self.start_canvas_uploads(
            uploads,
            request.scale_mode,
            super::Spec {
                zoom_out_ms,
                pan_ms,
                zoom_in_ms,
                easing,
                mode,
                walk,
                pan_axis,
                overview_scale,
                tile_count,
                started_at: Instant::now(),
                accelerated: false,
            },
        ) {
            Ok(started) => started,
            Err(response) => return response,
        };

        let queued_count = plan.queued.len();
        let (zoom_out_ms, pan_ms, zoom_in_ms) =
            accelerated_canvas_phases(zoom_out_ms, pan_ms, zoom_in_ms);
        self.enqueue_canvas_targets(
            plan.queued,
            request.scale_mode,
            &super::Queued {
                zoom_out_ms,
                pan_ms,
                zoom_in_ms,
                easing,
                mode,
                walk,
                pan_axis,
                overview_scale,
                tile_count,
                preview_paths: preview.paths.clone(),
                preview_start: preview.start_index,
            },
        );

        Response::Ack {
            message: format!(
                "started {started} output(s), queued {queued_count} output(s) with canvas transition"
            ),
        }
    }

    fn prepare_canvas_uploads(
        &mut self,
        starts: Vec<Target>,
        preview: &CanvasPreviewPlan,
        layout: CanvasLayoutSpec,
    ) -> Vec<CanvasUpload> {
        let mut uploads = Vec::with_capacity(starts.len());
        for target in starts {
            let surface_index = target.surface_index;
            let surface_name = target.name;
            let old_path = self.surfaces[surface_index].current_image.clone();
            let tiles = self.canvas_tiles_for_surface(
                surface_index,
                &preview.paths,
                preview.start_index,
                old_path.as_deref(),
                &target.image_path,
                layout,
            );
            let ready_tiles =
                canvas_ready_tile_count(&tiles, old_path.as_deref(), &target.image_path);
            trace_log!(
                self.trace,
                "set_canvas_wallpapers: prepared {surface_name} tiles={} ready={}",
                tiles.len(),
                ready_tiles
            );
            uploads.push(CanvasUpload {
                surface_index,
                image_path: target.image_path,
                decode_id: self.next_decode_id(),
                ready_tiles,
                tiles,
            });
        }
        uploads
    }

    fn schedule_canvas_target_decodes(&self, uploads: &[CanvasUpload]) -> Result<(), Response> {
        for upload in uploads {
            let job = DecodeJob {
                id: upload.decode_id,
                image_path: upload.image_path.clone(),
            };
            if let Err(error) = self.decode_tx.send(job) {
                return Err(Response::Error {
                    message: format!("decode worker is unavailable: {error}"),
                });
            }
        }
        Ok(())
    }

    fn start_canvas_uploads(
        &mut self,
        uploads: Vec<CanvasUpload>,
        scale_mode: ScaleMode,
        spec: super::Spec,
    ) -> Result<usize, Response> {
        let qh = self.qh.clone();
        let started = uploads.len();
        for upload in uploads {
            let surface_index = upload.surface_index;
            self.surfaces[surface_index].start_pending_canvas_transition(
                &self.egl,
                upload.image_path,
                scale_mode,
                upload.tiles,
                spec,
                upload.decode_id,
            );
            if let Err(message) = self.render_surface_active(surface_index, &qh) {
                self.surfaces[surface_index].mark_recreate_needed(
                    self.trace,
                    "canvas first frame",
                    &message,
                );
                return Err(Response::Error { message });
            }
        }
        Ok(started)
    }

    fn enqueue_canvas_targets(
        &mut self,
        targets: Vec<Target>,
        scale_mode: ScaleMode,
        queued: &super::Queued,
    ) {
        for target in targets {
            let surface_index = target.surface_index;
            let id = self.next_decode_id();
            self.surfaces[surface_index].enqueue_wallpaper_transition(
                QueuedWallpaper {
                    id,
                    image_path: target.image_path,
                    scale_mode,
                    transition: QueuedTransition::Canvas(queued.clone()),
                    state: QueuedState::Path,
                },
                &self.decode_tx,
            );
        }
    }

    fn canvas_tiles_for_surface(
        &mut self,
        surface_index: usize,
        preview_paths: &[String],
        preview_start: usize,
        old_path: Option<&str>,
        target_path: &str,
        layout: CanvasLayoutSpec,
    ) -> Vec<CanvasTile> {
        let pan_tiles = self.canvas_pan_tile_distance();
        self.surfaces[surface_index].canvas_tiles_for_preview_paths(
            &self.egl,
            &mut self.canvas_cache,
            CanvasTileBuild {
                preview_paths,
                preview_start,
                old_path,
                target_path,
                layout,
                pan_tiles,
            },
        )
    }
}
