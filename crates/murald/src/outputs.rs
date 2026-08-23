use std::collections::{BTreeSet, VecDeque};

use mural_ipc::ScaleMode;
use smithay_client_toolkit::reexports::protocols_wlr::output_power_management::v1::client::zwlr_output_power_v1::ZwlrOutputPowerV1;
use smithay_client_toolkit::shell::WaylandSurface as _;
use smithay_client_toolkit::shell::wlr_layer::{
    Anchor, KeyboardInteractivity, Layer, LayerSurface, LayerSurfaceConfigure,
};
use wayland_client::protocol::wl_output;
use wayland_client::{Proxy as _, QueueHandle};

use crate::MuralApp;
use crate::egl_render::Color;
use crate::output_power::OutputPowerData;
use crate::surface::{EglLifecycleState, OutputPowerState, OutputSurface};

impl MuralApp {
    pub(crate) fn sync_outputs(&mut self, qh: &QueueHandle<Self>) {
        let outputs = self
            .output_state
            .outputs()
            .filter_map(|output| self.output_state.info(&output).map(|info| (output, info)))
            .collect::<Vec<_>>();
        trace_log!(
            self.trace,
            "sync_outputs: compositor reports {} output(s); existing surfaces={}",
            outputs.len(),
            self.surfaces.len()
        );
        let live_outputs = outputs
            .iter()
            .map(|(output, _)| output.id().protocol_id())
            .collect::<BTreeSet<_>>();

        let mut removed = Vec::new();
        let mut index = 0;
        while index < self.surfaces.len() {
            if live_outputs.contains(&self.surfaces[index].output.id().protocol_id()) {
                index += 1;
            } else {
                let mut surface = self.surfaces.remove(index);
                surface.destroy(&self.egl);
                removed.push(surface.name);
            }
        }
        for name in removed {
            eprintln!("murald: removed output {name}");
        }

        for (output, info) in outputs {
            let name = output_name(&output, &info);
            if let Some(surface) = self
                .surfaces
                .iter_mut()
                .find(|surface| surface.output == output)
            {
                surface.name = name;
                let (x, y) = output_layout_position(&info);
                surface.layout_x = x;
                surface.layout_y = y;
                continue;
            }

            let layer = self.create_layer_surface(qh, &output, &name);
            let (output_power, power_state) = self.create_output_power(qh, &output, &name);
            let (layout_x, layout_y) = output_layout_position(&info);
            self.surfaces.push(OutputSurface {
                output,
                name: name.clone(),
                layout_x,
                layout_y,
                output_power,
                power_state,
                egl_window: None,
                egl_surface: None,
                layer,
                width: 0,
                height: 0,
                current_image: None,
                scale_mode: ScaleMode::Fill,
                clear_color: Color::default(),
                wallpaper: None,
                transition: None,
                queue: VecDeque::new(),
                frame_callback_pending: false,
                restore_pending: true,
                render_pending: false,
                egl_lifecycle: EglLifecycleState::Normal,
            });
            eprintln!("murald: created background surface for {name}");
        }
    }

    fn create_output_power(
        &self,
        qh: &QueueHandle<Self>,
        output: &wl_output::WlOutput,
        name: &str,
    ) -> (Option<ZwlrOutputPowerV1>, OutputPowerState) {
        let Some(manager) = &self.output_power_manager else {
            return (None, OutputPowerState::Unsupported);
        };

        let output_id = output.id().protocol_id();
        let power = manager.get_output_power(output, qh, OutputPowerData { output_id });
        trace_log!(self.trace, "output power watcher created for {name}");
        (Some(power), OutputPowerState::Unknown)
    }

    fn create_layer_surface(
        &self,
        qh: &QueueHandle<Self>,
        output: &wl_output::WlOutput,
        name: &str,
    ) -> LayerSurface {
        let wl_surface = self.compositor.create_surface(qh);
        let empty_region = self.compositor.wl_compositor().create_region(qh, ());
        wl_surface.set_input_region(Some(&empty_region));
        empty_region.destroy();

        let layer = self.layer_shell.create_layer_surface(
            qh,
            wl_surface,
            Layer::Background,
            Some("mural"),
            Some(output),
        );
        layer.set_anchor(Anchor::TOP | Anchor::RIGHT | Anchor::BOTTOM | Anchor::LEFT);
        layer.set_size(0, 0);
        layer.set_exclusive_zone(-1);
        layer.set_margin(0, 0, 0, 0);
        layer.set_keyboard_interactivity(KeyboardInteractivity::None);
        layer.commit();

        eprintln!("murald: committed initial layer-shell state for {name}");
        layer
    }

    pub(crate) fn configure_layer(
        &mut self,
        _qh: &QueueHandle<Self>,
        layer: &LayerSurface,
        configure: &LayerSurfaceConfigure,
    ) -> Result<(), String> {
        let trace = self.trace;
        let surface_index = self
            .surfaces
            .iter()
            .position(|surface| &surface.layer == layer)
            .ok_or_else(|| "configure event for unknown layer surface".to_owned())?;

        let width = i32::try_from(configure.new_size.0.max(1))
            .map_err(|_| "configured surface width is too large".to_owned())?;
        let height = i32::try_from(configure.new_size.1.max(1))
            .map_err(|_| "configured surface height is too large".to_owned())?;

        trace_log!(
            trace,
            "configure_layer: {} configured to {}x{}",
            self.surfaces[surface_index].name,
            width,
            height
        );
        if self.surfaces[surface_index].defer_configure(trace, width, height) {
            return Ok(());
        }
        self.surfaces[surface_index].ensure_egl_surface(&self.egl, width, height)?;
        self.surfaces[surface_index].render_pending = true;
        trace_log!(
            trace,
            "configure_layer: {} render deferred until after Wayland dispatch",
            self.surfaces[surface_index].name
        );
        Ok(())
    }
}

fn output_name(
    output: &wl_output::WlOutput,
    info: &smithay_client_toolkit::output::OutputInfo,
) -> String {
    info.name
        .clone()
        .unwrap_or_else(|| format!("wl_output@{}", output.id().protocol_id()))
}

fn output_layout_position(info: &smithay_client_toolkit::output::OutputInfo) -> (i32, i32) {
    info.logical_position.unwrap_or(info.location)
}
