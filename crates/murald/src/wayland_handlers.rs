use smithay_client_toolkit::compositor::CompositorHandler;
use smithay_client_toolkit::output::{OutputHandler, OutputState};
use smithay_client_toolkit::registry::{ProvidesRegistryState, RegistryState};
use smithay_client_toolkit::registry_handlers;
use smithay_client_toolkit::shell::WaylandSurface as _;
use smithay_client_toolkit::shell::wlr_layer::{
    LayerShellHandler, LayerSurface, LayerSurfaceConfigure,
};
use wayland_client::protocol::{wl_output, wl_surface};
use wayland_client::{Connection, QueueHandle};

use crate::MuralApp;

impl CompositorHandler for MuralApp {
    fn scale_factor_changed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _new_factor: i32,
    ) {
    }

    fn transform_changed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _new_transform: wl_output::Transform,
    ) {
    }

    fn frame(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        wl_surface: &wl_surface::WlSurface,
        _time: u32,
    ) {
        let Some(surface_index) = self
            .surfaces
            .iter()
            .position(|surface| surface.layer.wl_surface() == wl_surface)
        else {
            return;
        };

        self.surfaces[surface_index].frame_callback_pending = false;
        if self.surfaces[surface_index].transition.is_none() {
            return;
        }

        // Treat frame callbacks as pacing hints only. Rendering directly from
        // the Wayland callback can re-enter Mesa's Wayland EGL swap path while
        // libwayland is dispatching, which was one of the suspend/resume hang
        // shapes this renderer is designed to avoid. The main loop drains this
        // flag after the current Wayland dispatch returns.
        self.surfaces[surface_index].render_pending = true;
    }

    fn surface_enter(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _output: &wl_output::WlOutput,
    ) {
    }

    fn surface_leave(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _output: &wl_output::WlOutput,
    ) {
    }
}

impl OutputHandler for MuralApp {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }

    fn new_output(
        &mut self,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
        _output: wl_output::WlOutput,
    ) {
        trace_log!(self.trace, "wayland: new_output");
        self.sync_outputs(qh);
    }

    fn update_output(
        &mut self,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
        _output: wl_output::WlOutput,
    ) {
        trace_log!(self.trace, "wayland: update_output");
        self.sync_outputs(qh);
    }

    fn output_destroyed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        output: wl_output::WlOutput,
    ) {
        trace_log!(self.trace, "wayland: output_destroyed");
        let mut index = 0;
        while index < self.surfaces.len() {
            if self.surfaces[index].output == output {
                let mut surface = self.surfaces.remove(index);
                surface.destroy(&self.egl);
                eprintln!("murald: output destroyed: {}", surface.name);
            } else {
                index += 1;
            }
        }
    }
}

impl LayerShellHandler for MuralApp {
    fn closed(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, layer: &LayerSurface) {
        trace_log!(self.trace, "wayland: layer closed");
        if let Some(index) = self
            .surfaces
            .iter()
            .position(|surface| &surface.layer == layer)
        {
            let mut surface = self.surfaces.remove(index);
            surface.destroy(&self.egl);
            eprintln!(
                "murald: compositor closed layer surface for {}",
                surface.name
            );
        }
    }

    fn configure(
        &mut self,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
        layer: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _serial: u32,
    ) {
        trace_log!(self.trace, "wayland: layer configure");
        if let Err(error) = self.configure_layer(qh, layer, &configure) {
            eprintln!("murald: failed to configure layer surface: {error}");
        }
    }
}

impl ProvidesRegistryState for MuralApp {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }

    registry_handlers![OutputState];
}
