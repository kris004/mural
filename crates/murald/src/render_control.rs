use wayland_client::QueueHandle;

use crate::MuralApp;
use crate::decode::DecodeResult;
use crate::transitions::ActiveTransitionKind;

impl MuralApp {
    pub(crate) fn next_decode_id(&mut self) -> u64 {
        let id = self.next_decode_id;
        self.next_decode_id = self.next_decode_id.wrapping_add(1).max(1);
        id
    }

    pub(crate) fn render_surface_active(
        &mut self,
        surface_index: usize,
        qh: &QueueHandle<Self>,
    ) -> Result<(), String> {
        let pan_tiles = self.canvas_pan_tile_distance();
        let span = self.canvas_span_for_surface(surface_index);
        self.surfaces[surface_index].render_active(
            &self.egl,
            qh,
            &self.decode_tx,
            &mut self.canvas_cache,
            pan_tiles,
            span,
            self.trace,
        )
    }

    pub(crate) fn handle_decode_result(&mut self, result: DecodeResult) {
        if let Some(index) = self
            .surfaces
            .iter()
            .position(|surface| surface.has_active_canvas_decode(result.id))
        {
            match self.surfaces[index].accept_active_canvas_decode(&self.egl, result) {
                Ok(true) => {
                    let qh = self.qh.clone();
                    if let Err(error) = self.render_surface_active(index, &qh) {
                        eprintln!(
                            "murald: failed to render decoded canvas target for {}: {error}",
                            self.surfaces[index].name
                        );
                        self.surfaces[index].mark_recreate_needed(
                            self.trace,
                            "render decoded canvas target",
                            &error,
                        );
                        self.surfaces[index].restore_old_transition_wallpaper(&self.egl);
                    }
                }
                Ok(false) => {}
                Err(message) => {
                    eprintln!(
                        "murald: failed to prepare decoded canvas target for {}: {message}",
                        self.surfaces[index].name
                    );
                }
            }
            return;
        }

        if let Some(index) = self
            .surfaces
            .iter()
            .position(|surface| surface.has_queued_decode(result.id))
        {
            self.surfaces[index].accept_decode_result(result);
        }
    }

    pub(crate) fn pump_one_queue_upload(&mut self) {
        for surface in &mut self.surfaces {
            if surface.upload_one_queued_texture(&self.egl) {
                return;
            }
        }
    }

    pub(crate) fn render_pending_surfaces(&mut self, qh: &QueueHandle<Self>) {
        let trace = self.trace;
        for index in 0..self.surfaces.len() {
            if !self.surfaces[index].render_pending || !self.surfaces[index].egl_ready() {
                continue;
            }
            trace_log!(trace, "render_pending {}: start", self.surfaces[index].name);
            if self.surfaces[index].transition.is_some() {
                self.surfaces[index].upload_one_queued_texture(&self.egl);
            }
            if let Err(error) = self.render_surface_active(index, qh) {
                eprintln!(
                    "murald: failed to render pending wallpaper for {}: {error}",
                    self.surfaces[index].name
                );
                let pairwise_failed =
                    self.surfaces[index]
                        .transition
                        .as_ref()
                        .is_some_and(|transition| {
                            matches!(&transition.kind, ActiveTransitionKind::Pairwise(_))
                        });
                if pairwise_failed {
                    eprintln!(
                        "murald: deferred pairwise render failed; exiting renderer so committed state and queued work can be reconstructed"
                    );
                    self.flags.request_exit();
                    break;
                }
                self.surfaces[index].mark_recreate_needed(
                    trace,
                    "render pending wallpaper",
                    &error,
                );
                self.surfaces[index].settle_transition(&self.egl);
            }
            trace_log!(
                trace,
                "render_pending {}: done pending={}",
                self.surfaces[index].name,
                self.surfaces[index].render_pending
            );
        }
    }

    pub(crate) fn destroy_egl_surfaces(&mut self) {
        for surface in &mut self.surfaces {
            surface.destroy(&self.egl);
        }
    }
}
