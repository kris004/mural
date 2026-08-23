use std::sync::mpsc;

use calloop::channel as calloop_channel;
use mural_core::MuralConfig;
use mural_core::wallpaper::WallpaperControl;
use smithay_client_toolkit::compositor::CompositorState;
use smithay_client_toolkit::output::OutputState;
use smithay_client_toolkit::reexports::protocols_wlr::output_power_management::v1::client::zwlr_output_power_manager_v1::ZwlrOutputPowerManagerV1;
use smithay_client_toolkit::registry::RegistryState;
use smithay_client_toolkit::shell::wlr_layer::LayerShell;
use wayland_client::QueueHandle;

use crate::decode::DecodeJob;
use crate::egl_render::EglState;
use crate::surface::OutputSurface;
use crate::systemd_notify::SystemdNotify;
use crate::transitions::canvas::{CanvasCache, CanvasCacheResult};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TraceMode {
    Disabled,
    Enabled,
    Frames,
}

impl TraceMode {
    pub(crate) const fn enabled(self) -> bool {
        matches!(self, Self::Enabled | Self::Frames)
    }

    pub(crate) const fn frames_enabled(self) -> bool {
        matches!(self, Self::Frames)
    }
}

#[derive(Debug, Default)]
pub(crate) struct DaemonFlags {
    readiness: ReadinessState,
    startup: StartupState,
    cache_warm: CacheWarmState,
    shutdown: ShutdownState,
}

impl DaemonFlags {
    pub(crate) const fn ready_sent(&self) -> bool {
        matches!(self.readiness, ReadinessState::Sent)
    }

    pub(crate) fn mark_ready_sent(&mut self) {
        self.readiness = ReadinessState::Sent;
    }

    pub(crate) const fn startup_done(&self) -> bool {
        matches!(self.startup, StartupState::Done)
    }

    pub(crate) fn mark_startup_done(&mut self) {
        self.startup = StartupState::Done;
    }

    pub(crate) const fn cache_warm_done(&self) -> bool {
        matches!(self.cache_warm, CacheWarmState::Done)
    }

    pub(crate) fn mark_cache_warm_done(&mut self) {
        self.cache_warm = CacheWarmState::Done;
    }

    pub(crate) const fn should_exit(&self) -> bool {
        matches!(self.shutdown, ShutdownState::Requested)
    }

    pub(crate) fn request_exit(&mut self) {
        self.shutdown = ShutdownState::Requested;
    }
}

#[derive(Debug, Default)]
enum ReadinessState {
    #[default]
    Pending,
    Sent,
}

#[derive(Debug, Default)]
enum StartupState {
    #[default]
    Pending,
    Done,
}

#[derive(Debug, Default)]
enum CacheWarmState {
    #[default]
    Pending,
    Done,
}

#[derive(Debug, Default)]
enum ShutdownState {
    #[default]
    Running,
    Requested,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AppMode {
    Standalone,
    RendererChild,
}

impl AppMode {
    pub(crate) const fn is_renderer_child(self) -> bool {
        matches!(self, Self::RendererChild)
    }
}

pub(crate) struct MuralApp {
    pub(crate) mode: AppMode,
    pub(crate) registry_state: RegistryState,
    pub(crate) output_state: OutputState,
    pub(crate) qh: QueueHandle<MuralApp>,
    pub(crate) compositor: CompositorState,
    pub(crate) layer_shell: LayerShell,
    pub(crate) output_power_manager: Option<ZwlrOutputPowerManagerV1>,
    pub(crate) egl: EglState,
    pub(crate) decode_tx: mpsc::Sender<DecodeJob>,
    pub(crate) next_decode_id: u64,
    pub(crate) canvas_cache: Option<CanvasCache>,
    pub(crate) canvas_cache_result_tx: calloop_channel::Sender<CanvasCacheResult>,
    pub(crate) config: MuralConfig,
    pub(crate) wallpaper: WallpaperControl,
    pub(crate) notifier: SystemdNotify,
    pub(crate) flags: DaemonFlags,
    pub(crate) trace: TraceMode,
    pub(crate) next_ipc_id: u64,
    pub(crate) surfaces: Vec<OutputSurface>,
}
