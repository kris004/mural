pub(crate) mod canvas;
pub(crate) mod cut;
pub(crate) mod pairwise;
pub(crate) mod world;

use std::time::{Duration, Instant};

use khronos_egl as egl;
use mural_ipc::{Response, ScaleMode, SetRequest, Transition};

use crate::MuralApp;
use crate::decode::DecodedImage;
use crate::egl_render::{
    CanvasFrame, Color, EglState, FadeFrame, PushFrame, WallpaperTexture, WorldFrame,
};
use crate::surface::OutputPowerState;
use crate::transitions::canvas::{CanvasSpan, CanvasTile, CanvasUpload, canvas_phase_fractions};

pub(crate) enum ActiveTransitionKind {
    Pairwise(pairwise::Active),
    Canvas(canvas::Active),
    World(world::Active),
}

pub(crate) enum QueuedTransition {
    Pairwise(pairwise::Queued),
    Canvas(canvas::Queued),
    World(world::Queued),
}

pub(crate) struct ActiveTransition {
    pub(crate) old: Option<WallpaperTexture>,
    pub(crate) old_scale_mode: ScaleMode,
    pub(crate) new: Option<WallpaperTexture>,
    pub(crate) new_image: String,
    pub(crate) new_scale_mode: ScaleMode,
    pub(crate) transition: Transition,
    pub(crate) started_at: Instant,
    pub(crate) duration: Duration,
    pub(crate) kind: ActiveTransitionKind,
}

pub(crate) struct QueuedWallpaper {
    pub(crate) id: u64,
    pub(crate) image_path: String,
    pub(crate) scale_mode: ScaleMode,
    pub(crate) transition: QueuedTransition,
    pub(crate) state: QueuedState,
}

pub(crate) enum QueuedState {
    Path,
    Decoding,
    Decoded(DecodedImage),
    Uploaded(WallpaperTexture),
    Failed(String),
}

impl ActiveTransition {
    #[allow(clippy::cast_precision_loss)]
    fn progress(&self) -> f32 {
        if self.duration.is_zero() {
            return 1.0;
        }

        (self.started_at.elapsed().as_secs_f32() / self.duration.as_secs_f32()).clamp(0.0, 1.0)
    }

    pub(crate) fn render_progress(&self) -> f32 {
        let progress = self.progress();
        let ActiveTransitionKind::Canvas(canvas) = &self.kind else {
            return progress;
        };
        if canvas.target_decode_id.is_none() {
            return progress;
        }

        if self.new.is_some() {
            return progress;
        }

        let phase = canvas_phase_fractions(canvas.zoom_out_ms, canvas.pan_ms, canvas.zoom_in_ms);
        let pan_end = (phase.zoom_out + phase.pan).clamp(phase.zoom_out, 0.999);
        progress.min(pan_end)
    }

    pub(crate) fn retime_canvas_zoom_in_if_waiting(&mut self) {
        let ActiveTransitionKind::Canvas(canvas) = &self.kind else {
            return;
        };

        let phase = canvas_phase_fractions(canvas.zoom_out_ms, canvas.pan_ms, canvas.zoom_in_ms);
        let pan_end = (phase.zoom_out + phase.pan).clamp(phase.zoom_out, 0.999);
        if self.progress() <= pan_end {
            return;
        }

        let now = Instant::now();
        let elapsed_at_pan = self.duration.mul_f32(pan_end);
        self.started_at = now.checked_sub(elapsed_at_pan).unwrap_or(now);
    }

    pub(crate) fn accelerate_remaining(&mut self) {
        match &mut self.kind {
            ActiveTransitionKind::Pairwise(pairwise) => {
                if pairwise.accelerated {
                    return;
                }

                let elapsed = self.started_at.elapsed();
                let remaining = self.duration.saturating_sub(elapsed);
                self.duration = elapsed + accelerated_duration(remaining);
                self.transition = pairwise.effect.transition(self.duration);
                pairwise.accelerated = true;
            }
            ActiveTransitionKind::Canvas(canvas) => {
                if canvas.accelerated {
                    return;
                }

                let elapsed = self.started_at.elapsed();
                let remaining = self.duration.saturating_sub(elapsed);
                self.duration = elapsed + accelerated_duration(remaining);
                canvas.accelerated = true;
            }
            ActiveTransitionKind::World(world) => {
                if world.accelerated {
                    return;
                }

                let elapsed = self.started_at.elapsed();
                let remaining = self.duration.saturating_sub(elapsed);
                self.duration = elapsed + accelerated_duration(remaining);
                if let Transition::World { duration_ms, .. } = &mut self.transition {
                    *duration_ms = duration_millis(self.duration);
                }
                world.accelerated = true;
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn render_frame(
        &self,
        egl_state: &EglState,
        egl_surface: egl::Surface,
        width: i32,
        height: i32,
        clear_color: Color,
        progress: f32,
        span: Option<CanvasSpan>,
    ) -> Result<(), String> {
        match &self.kind {
            ActiveTransitionKind::Pairwise(pairwise) => {
                let Some(new) = self.new else {
                    return Err(format!(
                        "{} transition is missing target texture",
                        pairwise.effect.name()
                    ));
                };
                match pairwise.effect {
                    pairwise::Effect::Fade { easing } => egl_state.render_fade(
                        egl_surface,
                        width,
                        height,
                        FadeFrame {
                            old: self.old,
                            old_scale_mode: self.old_scale_mode,
                            new,
                            new_scale_mode: self.new_scale_mode,
                            clear_color,
                            easing,
                            progress,
                        },
                    ),
                    pairwise::Effect::Push {
                        direction,
                        easing,
                        mode,
                    } => egl_state.render_push(
                        egl_surface,
                        width,
                        height,
                        PushFrame {
                            old: self.old,
                            old_scale_mode: self.old_scale_mode,
                            new,
                            new_scale_mode: self.new_scale_mode,
                            clear_color,
                            direction,
                            easing,
                            mode,
                            progress,
                        },
                    ),
                }
            }
            ActiveTransitionKind::Canvas(canvas) => egl_state.render_canvas(
                egl_surface,
                width,
                height,
                &CanvasFrame {
                    old: self.old,
                    old_scale_mode: self.old_scale_mode,
                    new: self.new,
                    new_scale_mode: self.new_scale_mode,
                    clear_color,
                    easing: canvas.easing,
                    tiles: &canvas.tiles,
                    old_index: canvas.old_index,
                    target_index: canvas.target_index,
                    zoom_out_ms: canvas.zoom_out_ms,
                    pan_ms: canvas.pan_ms,
                    zoom_in_ms: canvas.zoom_in_ms,
                    mode: canvas.mode,
                    pan_axis: canvas.pan_axis,
                    overview_scale: canvas.overview_scale,
                    span,
                    progress,
                },
            ),
            ActiveTransitionKind::World(world) => {
                let Some(new) = self.new else {
                    return Err("world transition is missing target texture".to_owned());
                };
                egl_state.render_world(
                    egl_surface,
                    width,
                    height,
                    &WorldFrame {
                        old: self.old,
                        old_scale_mode: self.old_scale_mode,
                        new,
                        new_scale_mode: self.new_scale_mode,
                        clear_color,
                        easing: world.easing,
                        library_count: world.library_count,
                        columns: world.columns,
                        tile_cells: world.tile_cells,
                        route: world.route,
                        tiles: &world.tiles,
                        progress,
                    },
                )
            }
        }
    }
}

#[derive(Clone)]
pub(crate) struct Target {
    pub(crate) surface_index: usize,
    pub(crate) name: String,
    pub(crate) image_path: String,
}

pub(crate) struct TargetPlan {
    pub(crate) starts: Vec<Target>,
    pub(crate) queued: Vec<Target>,
}

impl MuralApp {
    pub(crate) fn skip_set_targets_if_all_power_off(
        &self,
        request: &SetRequest,
    ) -> Option<Response> {
        let names = self.set_target_names_if_all_power_off(request)?;
        Some(Response::Ack {
            message: format!(
                "skipped wallpaper set; all target outputs are off: {}",
                names.join(", ")
            ),
        })
    }

    pub(crate) fn ensure_set_targets_renderable(
        &self,
        request: &SetRequest,
    ) -> Result<(), Response> {
        let blocked = request
            .outputs
            .keys()
            .filter_map(|name| {
                self.surfaces
                    .iter()
                    .find(|surface| surface.name == *name && !surface.egl_ready())
                    .map(|surface| format!("{} ({})", surface.name, surface.power_state.name()))
            })
            .collect::<Vec<_>>();

        if blocked.is_empty() {
            return Ok(());
        }

        Err(Response::Error {
            message: format!(
                "output(s) not ready for EGL rendering: {}; try again after output power is on",
                blocked.join(", ")
            ),
        })
    }

    fn set_target_names_if_all_power_off(&self, request: &SetRequest) -> Option<Vec<String>> {
        if request.outputs.is_empty() {
            return None;
        }

        let mut names = Vec::with_capacity(request.outputs.len());
        for name in request.outputs.keys() {
            let surface = self.surfaces.iter().find(|surface| surface.name == *name)?;
            if surface.power_state != OutputPowerState::Off {
                return None;
            }
            names.push(name.clone());
        }
        Some(names)
    }

    pub(crate) fn plan_transition_targets(
        &self,
        request: &SetRequest,
        queue_active: bool,
    ) -> Result<TargetPlan, Response> {
        let unknown = request
            .outputs
            .keys()
            .filter(|name| !self.surfaces.iter().any(|surface| surface.name == **name))
            .cloned()
            .collect::<Vec<_>>();
        if !unknown.is_empty() && !request.allow_partial {
            return Err(Response::Error {
                message: format!("unknown output(s): {}", unknown.join(", ")),
            });
        }

        let mut starts = Vec::new();
        let mut queued = Vec::new();
        for (name, image_path) in &request.outputs {
            if unknown.contains(name) {
                continue;
            }

            let Some(surface_index) = self
                .surfaces
                .iter()
                .position(|surface| surface.name == *name)
            else {
                continue;
            };

            let target = Target {
                surface_index,
                name: name.clone(),
                image_path: image_path.clone(),
            };
            if queue_active && self.surfaces[surface_index].transition.is_some() {
                queued.push(target);
            } else {
                starts.push(target);
            }
        }

        Ok(TargetPlan { starts, queued })
    }
}

pub(crate) fn cleanup_canvas_uploads(egl_state: &EglState, uploads: Vec<CanvasUpload>) {
    for upload in uploads {
        delete_canvas_tiles(egl_state, upload.tiles);
    }
}

pub(crate) fn delete_transition_aux_textures(egl_state: &EglState, kind: ActiveTransitionKind) {
    match kind {
        ActiveTransitionKind::Pairwise(_) => {}
        ActiveTransitionKind::Canvas(canvas) => delete_canvas_tiles(egl_state, canvas.tiles),
        ActiveTransitionKind::World(world) => {
            for tile in world.tiles {
                egl_state.delete_texture(tile.texture);
            }
        }
    }
}

pub(crate) fn accelerated_duration(duration: Duration) -> Duration {
    if duration.is_zero() {
        return Duration::ZERO;
    }

    let millis = duration_millis(duration) / u64::from(crate::QUEUED_TRANSITION_SPEEDUP);
    Duration::from_millis(millis.max(1))
}

pub(crate) fn delete_canvas_tiles(egl_state: &EglState, tiles: Vec<CanvasTile>) {
    for tile in tiles {
        if let Some(texture) = tile.texture {
            egl_state.delete_texture(texture);
        }
    }
}

fn duration_millis(duration: Duration) -> u64 {
    duration.as_millis().try_into().unwrap_or(u64::MAX)
}
