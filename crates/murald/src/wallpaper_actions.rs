use mural_ipc::{OutputState as IpcOutputState, Response, WallpaperAction, WallpaperRequest};

use crate::MuralApp;
use crate::world_transition_not_ready_message;
use mural_core::wallpaper::ActiveOutput;

pub(crate) fn wallpaper_action_trace_name(action: &WallpaperAction) -> &'static str {
    match action {
        WallpaperAction::Next => "wallpaper.next",
        WallpaperAction::Back => "wallpaper.back",
        WallpaperAction::ShiftForward => "wallpaper.shift_forward",
        WallpaperAction::ShiftBack => "wallpaper.shift_back",
        WallpaperAction::Replace { .. } => "wallpaper.replace",
        WallpaperAction::Quarantine { .. } => "wallpaper.quarantine",
        WallpaperAction::Favorite { .. } => "wallpaper.favorite",
        WallpaperAction::Unfavorite { .. } => "wallpaper.unfavorite",
        WallpaperAction::Favorites => "wallpaper.favorites",
        WallpaperAction::Current => "wallpaper.current",
        WallpaperAction::Rescan => "wallpaper.rescan",
    }
}

impl MuralApp {
    pub(crate) fn handle_wallpaper_request(&mut self, request: &WallpaperRequest) -> Response {
        if self.mode.is_renderer_child() {
            return Response::Error {
                message: "wallpaper actions are handled by the murald supervisor".to_owned(),
            };
        }

        let action_name = wallpaper_action_trace_name(&request.action);
        trace_log!(self.trace, "wallpaper request {action_name}: start");
        let outputs = self.wallpaper_outputs();
        let result = match &request.action {
            WallpaperAction::Favorites => {
                trace_log!(self.trace, "wallpaper request {action_name}: favorites");
                Ok(self.wallpaper.favorites_response())
            }
            WallpaperAction::Current => {
                trace_log!(self.trace, "wallpaper request {action_name}: current");
                self.wallpaper.current_response(&outputs)
            }
            WallpaperAction::Rescan => {
                trace_log!(self.trace, "wallpaper request {action_name}: rescan");
                self.wallpaper.rescan_response()
            }
            WallpaperAction::Favorite { index } => {
                trace_log!(
                    self.trace,
                    "wallpaper request {action_name}: favorite index={index}"
                );
                self.wallpaper.favorite_action(&outputs, *index, true)
            }
            WallpaperAction::Unfavorite { index } => {
                trace_log!(
                    self.trace,
                    "wallpaper request {action_name}: unfavorite index={index}"
                );
                self.wallpaper.favorite_action(&outputs, *index, false)
            }
            _ => {
                let transition = request
                    .transition
                    .unwrap_or_else(|| self.config.transition_for_action(&request.action));
                if matches!(transition, mural_ipc::Transition::World { .. }) {
                    return Response::Error {
                        message: world_transition_not_ready_message(),
                    };
                }
                let scale_mode = request.scale_mode.unwrap_or(self.config.scale_mode);
                trace_log!(
                    self.trace,
                    "wallpaper request {action_name}: prepare change outputs={}",
                    outputs.len()
                );
                let capture_canvas_positions =
                    matches!(transition, mural_ipc::Transition::Canvas { .. });
                let prepared = match self.wallpaper.prepare_wallpaper_change(
                    &request.action,
                    &outputs,
                    capture_canvas_positions,
                ) {
                    Ok(prepared) => prepared,
                    Err(message) => return Response::Error { message },
                };
                trace_log!(
                    self.trace,
                    "wallpaper request {action_name}: prepared {} entries",
                    prepared.entries.len()
                );
                self.apply_prepared_wallpaper_change(prepared, transition, scale_mode)
            }
        };

        match result {
            Ok(response) => {
                trace_log!(self.trace, "wallpaper request {action_name}: complete ok");
                Response::Wallpaper(response)
            }
            Err(message) => {
                trace_log!(
                    self.trace,
                    "wallpaper request {action_name}: complete error: {message}"
                );
                Response::Error { message }
            }
        }
    }

    pub(crate) fn maybe_startup_display(&mut self) {
        if self.mode.is_renderer_child() {
            return;
        }
        if self.flags.startup_done() {
            self.maybe_notify_ready();
            self.maybe_warm_canvas_cache();
            return;
        }
        if self.surfaces.is_empty()
            || self
                .surfaces
                .iter()
                .any(|surface| surface.egl_surface.is_none())
        {
            return;
        }

        let outputs = self.wallpaper_outputs();
        let prepared = match self.wallpaper.prepare_startup_display(&outputs) {
            Ok(prepared) => prepared,
            Err(message) => {
                eprintln!("murald: startup wallpaper display skipped: {message}");
                self.flags.mark_startup_done();
                return;
            }
        };
        match self.apply_prepared_wallpaper_change(
            prepared,
            self.config.startup_transition(),
            self.config.scale_mode,
        ) {
            Ok(response) => {
                self.flags.mark_startup_done();
                eprintln!(
                    "murald: displayed {} startup wallpaper(s)",
                    response.entries.len()
                );
            }
            Err(message) => {
                self.flags.mark_startup_done();
                eprintln!("murald: startup wallpaper display failed: {message}");
            }
        }
        self.maybe_notify_ready();
        self.maybe_warm_canvas_cache();
    }

    pub(crate) fn maybe_notify_ready(&mut self) {
        if self.mode.is_renderer_child() {
            return;
        }
        if self.flags.ready_sent() {
            return;
        }
        if !self.surfaces.is_empty()
            && (self
                .surfaces
                .iter()
                .any(|surface| surface.egl_surface.is_none())
                || !self.flags.startup_done())
        {
            return;
        }
        self.notifier
            .ready(&format!("ready; {} output surface(s)", self.surfaces.len()));
        self.flags.mark_ready_sent();
        trace_log!(self.trace, "sd_notify READY sent");
    }

    pub(crate) fn maybe_restore_pending_wallpapers(&mut self) {
        if self.mode.is_renderer_child() {
            return;
        }
        if !self.flags.startup_done()
            || self.surfaces.is_empty()
            || !self.surfaces.iter().any(|surface| surface.restore_pending)
            || self
                .surfaces
                .iter()
                .any(|surface| surface.egl_surface.is_none())
        {
            return;
        }

        let pending = self
            .surfaces
            .iter()
            .filter(|surface| surface.restore_pending)
            .count();
        let outputs = self.wallpaper_outputs();
        let prepared = match self.wallpaper.prepare_startup_display(&outputs) {
            Ok(prepared) => prepared,
            Err(message) => {
                eprintln!("murald: pending wallpaper restore skipped: {message}");
                return;
            }
        };
        match self.apply_prepared_wallpaper_change(
            prepared,
            self.config.startup_transition(),
            self.config.scale_mode,
        ) {
            Ok(response) => {
                eprintln!(
                    "murald: restored {pending} pending wallpaper surface(s); displayed {} wallpaper(s)",
                    response.entries.len()
                );
            }
            Err(message) => {
                eprintln!("murald: pending wallpaper restore failed: {message}");
            }
        }
    }

    pub(crate) fn wallpaper_outputs(&self) -> Vec<ActiveOutput> {
        let mut outputs = self
            .surfaces
            .iter()
            .map(|surface| ActiveOutput {
                name: surface.name.clone(),
                x: surface.layout_x,
                y: surface.layout_y,
            })
            .collect::<Vec<_>>();
        outputs.sort_by(|left, right| {
            (left.x, left.y, left.name.as_str()).cmp(&(right.x, right.y, right.name.as_str()))
        });
        outputs
    }

    pub(crate) fn query_outputs(&self) -> Vec<IpcOutputState> {
        self.surfaces
            .iter()
            .map(|surface| IpcOutputState {
                name: surface.name.clone(),
                current_image: surface.current_image.clone(),
                scale_mode: surface.scale_mode,
                transition_state: surface.transition_state(),
                queue_depth: surface.queue.len(),
            })
            .collect()
    }
}
