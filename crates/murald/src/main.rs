use std::collections::BTreeMap;
use std::fs;
use std::os::fd::FromRawFd;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process;
use std::time::{Duration, Instant};

use calloop::EventLoop;
use calloop::channel as calloop_channel;
use calloop_wayland_source::WaylandSource;
use mural_ipc::Transition;
use smithay_client_toolkit::compositor::CompositorState;
use smithay_client_toolkit::output::OutputState;
use smithay_client_toolkit::reexports::protocols_wlr::output_power_management::v1::client::{
    zwlr_output_power_manager_v1::ZwlrOutputPowerManagerV1,
};
use smithay_client_toolkit::registry::RegistryState;
use smithay_client_toolkit::shell::wlr_layer::LayerShell;
use smithay_client_toolkit::{delegate_compositor, delegate_layer, delegate_output, delegate_registry};
use wayland_client::globals::registry_queue_init;
use wayland_client::protocol::wl_region;
use wayland_client::Connection;

macro_rules! trace_log {
    ($trace:expr, $($arg:tt)*) => {{
        if $trace.enabled() {
            eprintln!("murald trace: {}", format_args!($($arg)*));
        }
    }};
}

macro_rules! trace_frame_log {
    ($trace:expr, $($arg:tt)*) => {{
        if $trace.frames_enabled() {
            eprintln!("murald trace: {}", format_args!($($arg)*));
        }
    }};
}

mod app;
mod apply;
mod args;
mod cache_control;
mod canvas_control;
mod control_ipc;
mod decode;
mod egl_render;
mod event_sources;
mod health;
mod image_loader;
mod ipc;
mod library_watcher;
mod output_power;
mod outputs;
mod preload;
mod render_control;
mod supervisor;
mod surface;
mod systemd_notify;
mod transitions;
mod wallpaper_actions;
mod wayland_handlers;

pub(crate) use app::{AppMode, DaemonFlags, MuralApp, TraceMode};
use args::{RunMode, parse_args};
use control_ipc::{insert_renderer_control_source, write_frame};
use decode::spawn_decode_workers;
use egl_render::EglState;
use event_sources::{
    insert_canvas_cache_result_source, insert_decode_result_source, insert_watchdog_source,
};
use ipc::{bind_public_listener, insert_ipc_source};
use library_watcher::insert_library_watcher;
use mural_core::MuralConfig;
use mural_core::wallpaper::WallpaperControl;
use mural_core::world_cache::{
    ManifestState, world_cache_has_existing_tile_cache, world_cache_status,
};
use output_power::bind_output_power_manager;
use systemd_notify::SystemdNotify;
use transitions::canvas::CanvasCacheResult;

const QUEUED_TRANSITION_SPEEDUP: u32 = 4;
const MAX_PREPARED_PER_OUTPUT: usize = 4;
const MIN_CANVAS_READY_TILES: usize = 3;
const TRACE_LONG_DISPATCH: Duration = Duration::from_secs(5);

fn main() {
    if let Err(error) = run() {
        eprintln!("murald: {error}");
        process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let options = parse_args()?;
    match options.mode {
        RunMode::Supervisor => supervisor::run_supervisor(options.socket_path, options.trace),
        RunMode::Standalone => run_renderer(
            Some(&options.socket_path),
            options.trace,
            AppMode::Standalone,
            None,
        ),
        RunMode::RendererChild { fd } => {
            let control_stream = unsafe { UnixStream::from_raw_fd(fd) };
            run_renderer(
                None,
                options.trace,
                AppMode::RendererChild,
                Some(control_stream),
            )
        }
    }
}

#[allow(clippy::too_many_lines)]
fn run_renderer(
    socket_path: Option<&Path>,
    trace: TraceMode,
    mode: AppMode,
    mut control_stream: Option<UnixStream>,
) -> Result<(), String> {
    let listener = if let Some(socket_path) = socket_path {
        let listener = bind_public_listener(socket_path)?;
        listener
            .set_nonblocking(true)
            .map_err(|error| format!("failed to make IPC socket nonblocking: {error}"))?;
        Some(listener)
    } else {
        None
    };

    let conn = Connection::connect_to_env()
        .map_err(|error| format!("failed to connect to Wayland compositor: {error}"))?;
    let (globals, mut event_queue) = registry_queue_init(&conn)
        .map_err(|error| format!("failed to initialize Wayland registry: {error}"))?;
    let qh = event_queue.handle();

    let compositor = CompositorState::bind(&globals, &qh)
        .map_err(|error| format!("wl_compositor is not available: {error}"))?;
    let layer_shell = LayerShell::bind(&globals, &qh)
        .map_err(|error| format!("wlr-layer-shell is not available: {error}"))?;
    let output_power_manager = bind_output_power_manager(&globals, &qh, trace);
    let egl = EglState::new(&conn)?;
    let config = MuralConfig::load()?;
    if !mode.is_renderer_child() {
        validate_configured_world_cache(&config)?;
    }
    let (decode_tx, decoded_rx) = spawn_decode_workers(config.decode_full_workers)?;
    let (cache_result_tx, cache_result_rx) = calloop_channel::channel::<CanvasCacheResult>();
    let wallpaper = WallpaperControl::load(&config)?;
    let notifier = if mode.is_renderer_child() {
        SystemdNotify::disabled()
    } else {
        SystemdNotify::from_env()
    };

    let mut app = MuralApp {
        mode,
        registry_state: RegistryState::new(&globals),
        output_state: OutputState::new(&globals, &qh),
        qh: qh.clone(),
        compositor,
        layer_shell,
        output_power_manager,
        egl,
        decode_tx,
        next_decode_id: 1,
        canvas_cache: None,
        canvas_cache_result_tx: cache_result_tx,
        config,
        wallpaper,
        notifier,
        flags: DaemonFlags::default(),
        trace,
        next_ipc_id: 1,
        surfaces: Vec::new(),
    };
    trace_log!(
        app.trace,
        "enabled; mode={:?} socket={} wall_dir={} state_dir={:?}",
        mode,
        socket_path.map_or("<control-fd>".to_owned(), |path| path.display().to_string()),
        app.wallpaper.wall_dir().display(),
        app.config.state_dir
    );

    // Prime output metadata before entering the main loop. Output callbacks also
    // call sync_outputs(), so hotplug and late xdg-output metadata are handled.
    event_queue
        .roundtrip(&mut app)
        .map_err(|error| format!("initial Wayland roundtrip failed: {error}"))?;
    app.sync_outputs(&qh);

    let mut event_loop = EventLoop::<MuralApp>::try_new()
        .map_err(|error| format!("failed to create event loop: {error}"))?;
    let loop_handle = event_loop.handle();

    WaylandSource::new(conn, event_queue)
        .insert(loop_handle.clone())
        .map_err(|error| format!("failed to insert Wayland event source: {error}"))?;

    if let Some(listener) = listener {
        let Some(socket_path) = socket_path else {
            return Err("standalone IPC listener is missing its socket path".to_owned());
        };
        insert_ipc_source(&loop_handle, listener, socket_path)?;
        insert_watchdog_source(&loop_handle, SystemdNotify::watchdog_interval())?;
        insert_library_watcher(&loop_handle, app.wallpaper.wall_dir())?;
    }
    if let Some(stream) = control_stream.take() {
        write_frame(
            &stream,
            &mural_ipc::Response::Ack {
                message: "renderer ready".to_owned(),
            }
            .to_json(),
        )?;
        insert_renderer_control_source(&loop_handle, stream)?;
    }
    insert_decode_result_source(&loop_handle, decoded_rx)?;
    insert_canvas_cache_result_source(&loop_handle, cache_result_rx)?;

    match socket_path {
        Some(socket_path) => eprintln!(
            "murald: listening on {}; created {} layer-shell surface(s)",
            socket_path.display(),
            app.surfaces.len()
        ),
        None => eprintln!(
            "murald: renderer child ready; created {} layer-shell surface(s)",
            app.surfaces.len()
        ),
    }
    app.maybe_notify_ready();

    while !app.flags.should_exit() {
        let dispatch_start = Instant::now();
        event_loop
            .dispatch(None, &mut app)
            .map_err(|error| format!("event loop failed: {error}"))?;
        let dispatch_elapsed = dispatch_start.elapsed();
        if app.trace.enabled() && dispatch_elapsed >= TRACE_LONG_DISPATCH {
            eprintln!(
                "murald trace: dispatch returned after {:?}; exit={}",
                dispatch_elapsed,
                app.flags.should_exit()
            );
        }

        let qh = app.qh.clone();
        app.maybe_startup_display();
        app.maybe_restore_pending_wallpapers();
        app.render_pending_surfaces(&qh);
    }

    app.notifier.stopping();
    app.destroy_egl_surfaces();
    if let Some(socket_path) = socket_path {
        fs::remove_file(socket_path).map_err(|error| {
            format!(
                "failed to remove socket {} during shutdown: {error}",
                socket_path.display()
            )
        })?;
    }

    Ok(())
}

fn transition_name(transition: Transition) -> &'static str {
    match transition {
        Transition::Cut => "cut",
        Transition::Fade { .. } => "fade",
        Transition::World { .. } => "world",
        Transition::Push { .. } => "push",
        Transition::Canvas { .. } => "canvas",
    }
}

pub(crate) fn world_transition_not_ready_message() -> String {
    "world transitions require supervisor route planning and ready real world-cache coverage; use muralctl set or next/back/shift/replace/quarantine through the murald supervisor after running `muralctl world cache compute --scope all --background --progress`, or use cut, push, or canvas"
        .to_owned()
}

pub(crate) fn validate_configured_world_cache(config: &MuralConfig) -> Result<(), String> {
    if !config.uses_world_transition() {
        return Ok(());
    }

    let status = world_cache_status(config)?;
    if status.ready
        || matches!(
            status.manifest_state,
            ManifestState::Current | ManifestState::Stale
        ) && world_cache_has_existing_tile_cache(&status)
    {
        return Ok(());
    }

    Err(format!(
        "world transition is configured, but the world cache is not ready for the current ordered library: {}. Run: muralctl world cache compute --scope all --background --progress",
        status.message
    ))
}

fn validate_image_paths(outputs: &BTreeMap<String, String>) -> Result<(), String> {
    for (output, image_path) in outputs {
        let path = Path::new(image_path);
        if !path.is_file() {
            return Err(format!(
                "image path for output {output} does not exist or is not a file: {image_path}"
            ));
        }
    }

    Ok(())
}

delegate_compositor!(MuralApp);
delegate_output!(MuralApp);
delegate_layer!(MuralApp);
delegate_registry!(MuralApp);
wayland_client::delegate_noop!(MuralApp: ZwlrOutputPowerManagerV1);
wayland_client::delegate_noop!(MuralApp: wl_region::WlRegion);
