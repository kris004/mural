use std::time::{Duration, Instant};

use mural_ipc::{Easing, PushDirection, PushMode, Response, ScaleMode, SetRequest, Transition};

use crate::MuralApp;
use crate::egl_render::WallpaperTexture;
use crate::image_loader;
use crate::transitions::{
    QueuedState, QueuedTransition, QueuedWallpaper, Target, TargetPlan, accelerated_duration,
};

/// A compiled-in transition that renders a target from the current and next scenes.
///
/// Pairwise effects share decode, upload, queue, acceleration, and texture ownership.
/// Adding a new two-scene effect should normally extend this enum rather than copy
/// the lifecycle used by fade and push.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Effect {
    Fade {
        easing: Easing,
    },
    Push {
        direction: PushDirection,
        easing: Easing,
        mode: PushMode,
    },
}

impl Effect {
    pub(crate) const fn from_transition(transition: Transition) -> Option<(Self, u64)> {
        match transition {
            Transition::Fade {
                duration_ms,
                easing,
            } => Some((Self::Fade { easing }, duration_ms)),
            Transition::Push {
                direction,
                duration_ms,
                easing,
                mode,
            } => Some((
                Self::Push {
                    direction,
                    easing,
                    mode,
                },
                duration_ms,
            )),
            Transition::Cut | Transition::World { .. } | Transition::Canvas { .. } => None,
        }
    }

    pub(crate) fn transition(self, duration: Duration) -> Transition {
        let duration_ms = duration.as_millis().try_into().unwrap_or(u64::MAX);
        match self {
            Self::Fade { easing } => Transition::Fade {
                duration_ms,
                easing,
            },
            Self::Push {
                direction,
                easing,
                mode,
            } => Transition::Push {
                direction,
                duration_ms,
                easing,
                mode,
            },
        }
    }

    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Fade { .. } => "fade",
            Self::Push { .. } => "push",
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) struct Active {
    pub(crate) effect: Effect,
    pub(crate) accelerated: bool,
}

#[derive(Clone, Copy)]
pub(crate) struct Queued {
    pub(crate) effect: Effect,
    pub(crate) duration: Duration,
}

#[derive(Clone, Copy)]
pub(crate) struct Spec {
    pub(crate) effect: Effect,
    pub(crate) duration: Duration,
    pub(crate) started_at: Instant,
    pub(crate) accelerated: bool,
}

struct WallpaperUpload {
    surface_index: usize,
    image_path: String,
    texture: WallpaperTexture,
}

impl MuralApp {
    pub(crate) fn set_pairwise_wallpapers(
        &mut self,
        request: &SetRequest,
        plan: TargetPlan,
        effect: Effect,
        duration_ms: u64,
    ) -> Response {
        let duration = Duration::from_millis(duration_ms);
        trace_log!(
            self.trace,
            "set_pairwise_wallpapers: effect={} starts={} queued={}",
            effect.name(),
            plan.starts.len(),
            plan.queued.len()
        );

        let uploads = match self.upload_pairwise_start_textures(plan.starts) {
            Ok(uploads) => uploads,
            Err(response) => return response,
        };
        let started = match self.start_pairwise_uploads(
            uploads,
            request.scale_mode,
            Spec {
                effect,
                duration,
                started_at: Instant::now(),
                accelerated: false,
            },
        ) {
            Ok(started) => started,
            Err(response) => return response,
        };

        let queued_count = plan.queued.len();
        self.enqueue_pairwise_targets(
            plan.queued,
            request.scale_mode,
            Queued {
                effect,
                duration: accelerated_duration(duration),
            },
        );

        trace_log!(
            self.trace,
            "set_pairwise_wallpapers: complete effect={} started={started} queued={queued_count}",
            effect.name()
        );
        Response::Ack {
            message: format!(
                "started {started} output(s), queued {queued_count} output(s) with {} transition",
                effect.name()
            ),
        }
    }

    fn upload_pairwise_start_textures(
        &mut self,
        starts: Vec<Target>,
    ) -> Result<Vec<WallpaperUpload>, Response> {
        let mut uploads = Vec::with_capacity(starts.len());
        for target in starts {
            match self.upload_pairwise_start_texture(target) {
                Ok(upload) => uploads.push(upload),
                Err(response) => {
                    for upload in uploads {
                        self.egl.delete_texture(upload.texture);
                    }
                    return Err(response);
                }
            }
        }
        Ok(uploads)
    }

    fn upload_pairwise_start_texture(
        &mut self,
        target: Target,
    ) -> Result<WallpaperUpload, Response> {
        let surface_index = target.surface_index;
        let surface_name = target.name;
        trace_log!(self.trace, "set_pairwise_wallpapers: decode {surface_name}");
        let image = image_loader::load(&target.image_path)
            .map_err(|message| Response::Error { message })?;
        trace_log!(
            self.trace,
            "set_pairwise_wallpapers: decoded {surface_name}"
        );

        trace_log!(self.trace, "set_pairwise_wallpapers: upload {surface_name}");
        let texture = self.surfaces[surface_index]
            .upload_wallpaper_texture(&self.egl, &image)
            .map_err(|message| Response::Error { message })?;
        trace_log!(
            self.trace,
            "set_pairwise_wallpapers: uploaded {surface_name}"
        );
        Ok(WallpaperUpload {
            surface_index,
            image_path: target.image_path,
            texture,
        })
    }

    fn start_pairwise_uploads(
        &mut self,
        uploads: Vec<WallpaperUpload>,
        scale_mode: ScaleMode,
        spec: Spec,
    ) -> Result<usize, Response> {
        let qh = self.qh.clone();
        let started = uploads.len();
        let mut uploads = uploads.into_iter();
        let mut started_surfaces: Vec<usize> = Vec::with_capacity(started);
        while let Some(upload) = uploads.next() {
            let surface_index = upload.surface_index;
            trace_log!(
                self.trace,
                "set_pairwise_wallpapers: start {} transition {}",
                spec.effect.name(),
                self.surfaces[surface_index].name
            );
            self.surfaces[surface_index].start_pairwise_transition(
                &self.egl,
                upload.image_path,
                upload.texture,
                scale_mode,
                spec,
            );
            if let Err(message) = self.render_surface_active(surface_index, &qh) {
                self.surfaces[surface_index].restore_old_transition_wallpaper(&self.egl);
                for previous_index in &started_surfaces {
                    self.surfaces[*previous_index].restore_old_transition_wallpaper(&self.egl);
                }
                for remaining in uploads {
                    self.egl.delete_texture(remaining.texture);
                }
                for previous_index in started_surfaces {
                    if let Err(rollback_error) = self.render_surface_active(previous_index, &qh) {
                        self.surfaces[previous_index].mark_recreate_needed(
                            self.trace,
                            "pairwise batch rollback",
                            &rollback_error,
                        );
                    }
                }
                self.surfaces[surface_index].mark_recreate_needed(
                    self.trace,
                    "pairwise first frame",
                    &message,
                );
                return Err(Response::Error { message });
            }
            started_surfaces.push(surface_index);
        }
        Ok(started)
    }

    fn enqueue_pairwise_targets(
        &mut self,
        targets: Vec<Target>,
        scale_mode: ScaleMode,
        queued: Queued,
    ) {
        for target in targets {
            let surface_index = target.surface_index;
            let id = self.next_decode_id();
            trace_log!(
                self.trace,
                "set_pairwise_wallpapers: enqueue {} effect={} id={id}",
                self.surfaces[surface_index].name,
                queued.effect.name()
            );
            self.surfaces[surface_index].enqueue_wallpaper_transition(
                QueuedWallpaper {
                    id,
                    image_path: target.image_path,
                    scale_mode,
                    transition: QueuedTransition::Pairwise(queued),
                    state: QueuedState::Path,
                },
                &self.decode_tx,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use mural_ipc::{Easing, PushDirection, PushMode, Transition};

    use super::Effect;

    #[test]
    fn effects_round_trip_to_typed_transitions() {
        let fade = Transition::Fade {
            duration_ms: 420,
            easing: Easing::Linear,
        };
        let push = Transition::Push {
            direction: PushDirection::Left,
            duration_ms: 700,
            easing: Easing::EaseInOutCubic,
            mode: PushMode::Screen,
        };

        for transition in [fade, push] {
            let (effect, duration_ms) = Effect::from_transition(transition).unwrap();
            assert_eq!(
                effect.transition(Duration::from_millis(duration_ms)),
                transition
            );
        }
    }

    #[test]
    fn scene_and_immediate_transitions_are_not_pairwise() {
        assert!(Effect::from_transition(Transition::Cut).is_none());
    }
}
