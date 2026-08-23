use std::collections::{BTreeMap, BTreeSet};

use mural_ipc::{ClearRequest, Response, ScaleMode, SetRequest, Transition};

use crate::egl_render::Color;
use crate::transitions::pairwise::Effect as PairwiseEffect;
use crate::{MuralApp, transition_name, validate_image_paths, world_transition_not_ready_message};
use mural_core::wallpaper::PreparedWallpaperChange;

impl MuralApp {
    pub(crate) fn apply_prepared_wallpaper_change(
        &mut self,
        mut prepared: PreparedWallpaperChange,
        transition: Transition,
        scale_mode: ScaleMode,
    ) -> Result<mural_ipc::WallpaperResponse, String> {
        if matches!(transition, Transition::World { .. }) {
            return Err(world_transition_not_ready_message());
        }
        debug_assert_eq!(prepared.entries.len(), prepared.selection.len());
        let canvas_preview = self.canvas_preview_for_prepared_change(&prepared, transition)?;
        trace_log!(
            self.trace,
            "apply wallpaper change {}: move_quarantine",
            prepared.action
        );
        self.wallpaper.move_quarantine(&mut prepared)?;
        let outputs = prepared
            .entries
            .iter()
            .map(|entry| (entry.output.clone(), entry.path.clone()))
            .collect::<BTreeMap<_, _>>();
        trace_log!(
            self.trace,
            "apply wallpaper change {}: set_wallpapers outputs={} transition={}",
            prepared.action,
            outputs.len(),
            transition_name(transition)
        );
        if let Some(preview) = &canvas_preview {
            let _scheduled = self.schedule_canvas_cache_warm(
                preview.paths.clone(),
                1,
                self.config.canvas_cache_backend,
            );
        }
        let request = SetRequest {
            outputs,
            transition,
            scale_mode,
            allow_partial: false,
        };
        let response = if let Some(preview) = canvas_preview {
            self.set_canvas_wallpapers_from_preview(&request, &preview)
        } else {
            self.set_wallpapers(&request)
        };
        if let Response::Error { message } = response {
            trace_log!(
                self.trace,
                "apply wallpaper change {}: set_wallpapers error: {message}",
                prepared.action
            );
            self.wallpaper.rollback_wallpaper_change(prepared);
            return Err(message);
        }
        trace_log!(
            self.trace,
            "apply wallpaper change {}: commit state",
            prepared.action
        );
        self.wallpaper.commit_wallpaper_change(prepared)
    }

    pub(crate) fn set_wallpapers(&mut self, request: &SetRequest) -> Response {
        trace_log!(
            self.trace,
            "set_wallpapers: start outputs={} transition={}",
            request.outputs.len(),
            transition_name(request.transition)
        );
        if matches!(request.transition, Transition::Canvas { .. }) {
            return Response::Error {
                message: "canvas transitions require mural wallpaper action history and cannot be used with explicit set; use next/back/shift/replace/quarantine with --transition canvas, or use set with cut or push"
                    .to_owned(),
            };
        }
        if matches!(request.transition, Transition::World { .. }) {
            return Response::Error {
                message: world_transition_not_ready_message(),
            };
        }
        if let Err(message) = validate_image_paths(&request.outputs) {
            return Response::Error { message };
        }
        if let Some(response) = self.skip_set_targets_if_all_power_off(request) {
            return response;
        }
        if let Err(response) = self.ensure_set_targets_renderable(request) {
            return response;
        }

        if let Some((effect, duration_ms)) = PairwiseEffect::from_transition(request.transition) {
            let plan = match self.plan_transition_targets(request, true) {
                Ok(plan) => plan,
                Err(response) => return response,
            };
            return self.set_pairwise_wallpapers(request, plan, effect, duration_ms);
        }

        let plan = match self.plan_transition_targets(request, false) {
            Ok(plan) => plan,
            Err(response) => return response,
        };
        self.set_cut_wallpapers(request, plan)
    }

    pub(crate) fn clear(&mut self, request: ClearRequest) -> Response {
        let color = match Color::parse(&request.color) {
            Ok(color) => color,
            Err(message) => return Response::Error { message },
        };

        let targets = if request.outputs.is_empty() {
            self.surfaces
                .iter()
                .map(|surface| surface.name.clone())
                .collect::<Vec<_>>()
        } else {
            request.outputs
        };

        let mut cleared = 0_usize;
        for name in &targets {
            if let Some(surface) = self
                .surfaces
                .iter_mut()
                .find(|surface| surface.name == *name)
            {
                surface.clear_queue(&self.egl);
                surface.discard_transition(&self.egl);
                surface.current_image = None;
                surface.restore_pending = false;
                surface.clear_color = color;
                if let Err(error) = surface.render_clear(&self.egl, self.trace) {
                    surface.mark_recreate_needed(self.trace, "clear render", &error);
                    return Response::Error { message: error };
                }
                if let Some(texture) = surface.wallpaper.take() {
                    self.egl.delete_texture(texture);
                }
                cleared += 1;
            }
        }

        if cleared != targets.len() {
            let known = self
                .surfaces
                .iter()
                .map(|surface| surface.name.as_str())
                .collect::<BTreeSet<_>>();
            let unknown = targets
                .iter()
                .filter(|target| !known.contains(target.as_str()))
                .cloned()
                .collect::<Vec<_>>();
            return Response::Error {
                message: format!("unknown output(s): {}", unknown.join(", ")),
            };
        }

        Response::Ack {
            message: format!("cleared {cleared} output(s) to {}", request.color),
        }
    }
}
