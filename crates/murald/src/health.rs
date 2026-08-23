use mural_ipc::{HealthOutput, HealthResponse};

use crate::MuralApp;

impl MuralApp {
    pub(crate) fn renderer_health_response(&self) -> HealthResponse {
        HealthResponse {
            role: "renderer".to_owned(),
            supervisor_pid: None,
            renderer_pid: Some(std::process::id()),
            renderer_generation: 0,
            renderer_state: "running".to_owned(),
            restart_count: 0,
            last_error: None,
            last_diagnostic: None,
            outputs: self
                .surfaces
                .iter()
                .map(|surface| HealthOutput {
                    name: surface.name.clone(),
                    layout_x: surface.layout_x,
                    layout_y: surface.layout_y,
                    width: surface.width,
                    height: surface.height,
                    power_state: surface.power_state.name().to_owned(),
                    render_state: surface.render_state_name().to_owned(),
                    restore_pending: surface.restore_pending,
                    current_image: surface.current_image.clone(),
                    transition_target_image: surface
                        .transition
                        .as_ref()
                        .map(|transition| transition.new_image.clone()),
                    scale_mode: surface.scale_mode,
                    transition_state: surface.transition_state(),
                    queue_depth: surface.queue.len(),
                    frame_callback_pending: surface.frame_callback_pending,
                    render_pending: surface.render_pending,
                })
                .collect(),
        }
    }
}
