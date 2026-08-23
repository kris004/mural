use smithay_client_toolkit::reexports::protocols_wlr::output_power_management::v1::client::{
    zwlr_output_power_manager_v1::ZwlrOutputPowerManagerV1,
    zwlr_output_power_v1::{self, ZwlrOutputPowerV1},
};
use wayland_client::globals::GlobalList;
use wayland_client::{Connection, Dispatch, Proxy as _, QueueHandle, WEnum};

use crate::surface::OutputPowerState;
use crate::{MuralApp, TraceMode};

pub(crate) struct OutputPowerData {
    pub(crate) output_id: u32,
}

pub(crate) fn bind_output_power_manager(
    globals: &GlobalList,
    qh: &QueueHandle<MuralApp>,
    trace: TraceMode,
) -> Option<ZwlrOutputPowerManagerV1> {
    match globals.bind::<ZwlrOutputPowerManagerV1, _, _>(qh, 1..=1, ()) {
        Ok(manager) => Some(manager),
        Err(error) => {
            trace_log!(
                trace,
                "wlr output power management unavailable; DPMS guard disabled: {error}"
            );
            None
        }
    }
}

impl MuralApp {
    pub(crate) fn handle_output_power_mode(
        &mut self,
        output_id: u32,
        mode: WEnum<zwlr_output_power_v1::Mode>,
    ) {
        let state = match mode {
            WEnum::Value(zwlr_output_power_v1::Mode::On) => OutputPowerState::On,
            WEnum::Value(zwlr_output_power_v1::Mode::Off) => OutputPowerState::Off,
            WEnum::Unknown(value) => {
                trace_log!(
                    self.trace,
                    "output power watcher wl_output@{output_id}: unknown mode {value}"
                );
                OutputPowerState::Unknown
            }
            WEnum::Value(_) => OutputPowerState::Unknown,
        };

        let Some(index) = self
            .surfaces
            .iter()
            .position(|surface| surface.output.id().protocol_id() == output_id)
        else {
            trace_log!(
                self.trace,
                "output power watcher wl_output@{output_id}: mode for unknown output"
            );
            return;
        };

        let previous = self.surfaces[index].power_state;
        self.surfaces[index].power_state = state;
        if state == OutputPowerState::Off {
            self.surfaces[index].render_pending = true;
        }
        trace_log!(
            self.trace,
            "output power watcher {}: {} -> {}",
            self.surfaces[index].name,
            previous.name(),
            state.name()
        );

        // Rendering pending surfaces is intentionally done by the main loop
        // after the current Wayland dispatch returns. Calling eglSwapBuffers()
        // directly from output-power/configure handlers can re-enter
        // libwayland and block the daemon event loop during resume.
    }

    pub(crate) fn handle_output_power_failed(&mut self, output_id: u32) {
        let Some(index) = self
            .surfaces
            .iter()
            .position(|surface| surface.output.id().protocol_id() == output_id)
        else {
            trace_log!(
                self.trace,
                "output power watcher wl_output@{output_id}: failed for unknown output"
            );
            return;
        };

        let name = self.surfaces[index].name.clone();
        if let Some(power) = self.surfaces[index].output_power.take() {
            power.destroy();
        }
        self.surfaces[index].power_state = OutputPowerState::Unsupported;
        trace_log!(
            self.trace,
            "output power watcher {name}: failed; DPMS guard disabled for this output"
        );
        // See handle_output_power_mode(): the main loop drains pending renders.
    }
}

impl Dispatch<ZwlrOutputPowerV1, OutputPowerData> for MuralApp {
    fn event(
        state: &mut Self,
        _proxy: &ZwlrOutputPowerV1,
        event: zwlr_output_power_v1::Event,
        data: &OutputPowerData,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_output_power_v1::Event::Mode { mode } => {
                state.handle_output_power_mode(data.output_id, mode);
            }
            zwlr_output_power_v1::Event::Failed => {
                state.handle_output_power_failed(data.output_id);
            }
            _ => {}
        }
    }
}
