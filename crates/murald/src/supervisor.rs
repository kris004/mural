use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs;
use std::io::Write;
use std::net::Shutdown;
use std::os::fd::AsRawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use mural_core::MuralConfig;
use mural_core::wallpaper::{ActiveOutput, PreparedWallpaperChange, WallpaperControl};
use mural_core::world_cache::{
    DEFAULT_WORLD_CELL_THUMBNAIL_EDGE, DEFAULT_WORLD_TILE_CELLS, ManifestState, WorldCacheSnapshot,
    WorldCacheStatus, WorldLodCacheStatus, WorldTileCacheEntry, read_indexed_world_cache_snapshot,
    world_cache_snapshot_for_library, world_cache_status, world_lod_cache_statuses,
    world_lod_tile_cells, world_tile_pyramid_cache_entries_for_snapshot,
};
use mural_ipc::{
    CanvasPanAxis, CapabilitiesResponse, DaemonMode, HealthOutput, HealthResponse,
    PROTOCOL_VERSION, RenderCanvasSetRequest, RenderWorldSetRequest, Request, Response, SetRequest,
    Transition, WallpaperAction, WallpaperEntry, WallpaperRequest, WallpaperResponse,
    WorldRouteFocus, parse_health_response, parse_public_request, response_error_message,
    response_is_error,
};
use mural_render::{
    Size, WorldLayout, WorldRouteLodCandidate, WorldSnapshot, world_route_lod_for_budget,
    world_tiles_for_route,
};

use crate::control_ipc::{read_frame, write_frame};
use crate::ipc::{bind_public_listener, read_public_request_json};
use crate::library_watcher::LibraryWatcher;
use crate::systemd_notify::SystemdNotify;
use crate::transitions::canvas::resolve_canvas_tile_count_for_pan;
use crate::{TraceMode, transition_name, validate_configured_world_cache, validate_image_paths};

const PUBLIC_READ_TIMEOUT: Duration = Duration::from_secs(2);
const PUBLIC_IDLE_SLEEP: Duration = Duration::from_millis(50);
const RENDERER_READY_TIMEOUT: Duration = Duration::from_secs(10);
const RENDERER_HEALTH_TIMEOUT: Duration = Duration::from_secs(2);
const RENDERER_RENDER_TIMEOUT: Duration = Duration::from_secs(10);
const RENDERER_EXIT_TIMEOUT: Duration = Duration::from_secs(2);
const DIAGNOSTIC_TIMEOUT: Duration = Duration::from_secs(3);
const RESTORE_PENDING_CHECK_INTERVAL: Duration = Duration::from_millis(500);
const WORLD_ROUTE_WARMUP_DEDUPE_TTL: Duration = Duration::from_mins(30);
const MAX_WORLD_ROUTE_TILES: usize = 16;
const MAX_AUTO_WORLD_ROUTE_WARMUP_TILES: usize = 4;
const MAX_AUTO_WORLD_ROUTE_WARMUP_LOD: usize = 0;
const WORLD_NEAR_FUTURE_WARMUP_COUNT: usize = 1;
const WORLD_NEIGHBORHOOD_WARMUP_TILE_LIMIT: usize = 4;

struct WorldRouteCachePlan {
    snapshot: WorldCacheSnapshot,
    routes: BTreeMap<String, WorldRouteFocus>,
    tile_entries: Vec<WorldTileCacheEntry>,
}

struct WorldRouteCacheError {
    message: String,
    allow_fallback: bool,
    auto_warmup: Option<WorldRouteWarmupEstimate>,
}

impl WorldRouteCacheError {
    fn hard(message: String) -> Self {
        Self {
            message,
            allow_fallback: false,
            auto_warmup: None,
        }
    }

    fn fallback(message: String) -> Self {
        Self {
            message,
            allow_fallback: true,
            auto_warmup: None,
        }
    }

    fn fallback_with_warmup(message: String, estimate: WorldRouteWarmupEstimate) -> Self {
        Self {
            message,
            allow_fallback: true,
            auto_warmup: Some(estimate),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct WorldRouteWarmupEstimate {
    tile_count: usize,
    max_lod: usize,
}

impl WorldRouteWarmupEstimate {
    fn auto_skip_reason(self) -> Option<String> {
        if self.max_lod > MAX_AUTO_WORLD_ROUTE_WARMUP_LOD {
            return Some(format!(
                "auto route warmup skipped: selected LOD {} exceeds automatic warmup limit LOD {}; run the command manually if you want this larger warmup",
                self.max_lod, MAX_AUTO_WORLD_ROUTE_WARMUP_LOD
            ));
        }
        if self.tile_count > MAX_AUTO_WORLD_ROUTE_WARMUP_TILES {
            return Some(format!(
                "auto route warmup skipped: missing {} route tile(s), automatic limit is {}; run the command manually if you want this larger warmup",
                self.tile_count, MAX_AUTO_WORLD_ROUTE_WARMUP_TILES
            ));
        }
        None
    }
}

pub(crate) fn run_supervisor(socket_path: PathBuf, trace: TraceMode) -> Result<(), String> {
    let listener = bind_public_listener(&socket_path)?;
    listener
        .set_nonblocking(true)
        .map_err(|error| format!("failed to make IPC socket nonblocking: {error}"))?;

    let config = MuralConfig::load()?;
    validate_configured_world_cache(&config)?;
    let wallpaper = WallpaperControl::load(&config)?;
    let library_watcher = match LibraryWatcher::new(wallpaper.wall_dir()) {
        Ok(watcher) => Some(watcher),
        Err(error) => {
            eprintln!("murald: wallpaper directory watcher disabled: {error}");
            None
        }
    };
    let notifier = SystemdNotify::from_env();
    let mut supervisor = Supervisor {
        socket_path,
        listener,
        trace,
        config,
        wallpaper,
        library_watcher,
        notifier,
        renderer: None,
        next_generation: 1,
        restart_count: 0,
        last_error: None,
        last_diagnostic: None,
        next_ipc_id: 1,
        should_exit: false,
        ready_sent: false,
        last_watchdog: Instant::now(),
        last_restore_check: Instant::now(),
        scheduled_world_warmups: BTreeMap::new(),
    };

    supervisor.spawn_renderer()?;
    supervisor.wait_renderer_ready("startup");
    supervisor.restore_current_wallpapers("startup");
    supervisor.notify_ready();
    supervisor.run_loop()
}

struct Supervisor {
    socket_path: PathBuf,
    listener: UnixListener,
    trace: TraceMode,
    config: MuralConfig,
    wallpaper: WallpaperControl,
    library_watcher: Option<LibraryWatcher>,
    notifier: SystemdNotify,
    renderer: Option<RendererProcess>,
    next_generation: u64,
    restart_count: u64,
    last_error: Option<String>,
    last_diagnostic: Option<String>,
    next_ipc_id: u64,
    should_exit: bool,
    ready_sent: bool,
    last_watchdog: Instant,
    last_restore_check: Instant,
    scheduled_world_warmups: BTreeMap<String, Instant>,
}

impl Supervisor {
    fn run_loop(&mut self) -> Result<(), String> {
        eprintln!(
            "murald: supervisor listening on {}; renderer pid={}",
            self.socket_path.display(),
            self.renderer_pid()
                .map_or_else(|| "none".to_owned(), |pid| pid.to_string())
        );

        while !self.should_exit {
            self.ensure_renderer()?;
            self.poll_library_watcher();
            self.maybe_restore_pending_renderer_surfaces();
            self.accept_public_connections();
            self.maybe_watchdog();
            thread::sleep(PUBLIC_IDLE_SLEEP);
        }

        self.notifier.stopping();
        self.stop_renderer();
        fs::remove_file(&self.socket_path).map_err(|error| {
            format!(
                "failed to remove socket {} during shutdown: {error}",
                self.socket_path.display()
            )
        })?;
        Ok(())
    }

    fn notify_ready(&mut self) {
        if self.ready_sent {
            return;
        }
        self.notifier.ready(&format!(
            "supervisor ready; renderer pid={}",
            self.renderer_pid()
                .map_or_else(|| "none".to_owned(), |pid| pid.to_string())
        ));
        self.ready_sent = true;
    }

    fn maybe_watchdog(&mut self) {
        let Some(interval) = SystemdNotify::watchdog_interval() else {
            return;
        };
        if self.last_watchdog.elapsed() >= interval {
            self.notifier.watchdog();
            self.last_watchdog = Instant::now();
        }
    }

    fn poll_library_watcher(&mut self) {
        let Some(watcher) = &self.library_watcher else {
            return;
        };
        let paths = match watcher.drain_events() {
            Ok(paths) => paths,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return,
            Err(error) => {
                eprintln!("murald: failed to read wallpaper directory events: {error}");
                return;
            }
        };
        for path in paths {
            match self.wallpaper.add_top_level_file(&path) {
                Ok(true) => eprintln!("murald: added wallpaper {}", path.display()),
                Ok(false) => {}
                Err(error) => {
                    eprintln!(
                        "murald: failed to add wallpaper {}: {error}",
                        path.display()
                    );
                }
            }
        }
    }

    fn accept_public_connections(&mut self) {
        loop {
            match self.listener.accept() {
                Ok((mut stream, _addr)) => {
                    let connection_id = self.next_ipc_id();
                    trace_log!(self.trace, "supervisor ipc #{connection_id}: accepted");
                    if let Err(error) = stream.set_read_timeout(Some(PUBLIC_READ_TIMEOUT)) {
                        eprintln!("murald: failed to set IPC read timeout: {error}");
                    }
                    let should_stop = self.handle_public_connection(&mut stream, connection_id);
                    if should_stop {
                        self.should_exit = true;
                        return;
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return,
                Err(error) => {
                    eprintln!(
                        "murald: failed to accept IPC connection on {}: {error}",
                        self.socket_path.display()
                    );
                    return;
                }
            }
        }
    }

    fn handle_public_connection(&mut self, stream: &mut UnixStream, connection_id: u64) -> bool {
        let (response_json, should_stop) = match read_public_request_json(stream) {
            Ok((request_json, bytes_read)) => {
                trace_log!(
                    self.trace,
                    "supervisor ipc #{connection_id}: read {bytes_read} byte(s)"
                );
                match parse_public_request(&request_json) {
                    Ok(request) => self.handle_public_request(request),
                    Err(error) => (
                        Response::Error {
                            message: error.to_string(),
                        }
                        .to_json(),
                        false,
                    ),
                }
            }
            Err(error) => (
                Response::Error {
                    message: format!("failed to read request: {error}"),
                }
                .to_json(),
                false,
            ),
        };

        if let Err(error) = stream.write_all(response_json.as_bytes()) {
            eprintln!("murald: failed to write IPC response: {error}");
        }
        let _ = stream.shutdown(Shutdown::Both);
        should_stop
    }

    fn handle_public_request(&mut self, request: Request) -> (String, bool) {
        match request {
            Request::Ping => (
                Response::Pong {
                    version: PROTOCOL_VERSION,
                }
                .to_json(),
                false,
            ),
            Request::Capabilities => (
                Response::Capabilities(CapabilitiesResponse::current(DaemonMode::Supervisor))
                    .to_json(),
                false,
            ),
            Request::Health => (
                Response::Health(Box::new(self.health_response())).to_json(),
                false,
            ),
            Request::Wallpaper(request) => {
                (self.handle_wallpaper_request(&request).to_json(), false)
            }
            Request::Set(request) if matches!(request.transition, Transition::World { .. }) => {
                (self.handle_world_set_request(&request), false)
            }
            Request::Stop => (
                Response::Ack {
                    message: "stopping".to_owned(),
                }
                .to_json(),
                true,
            ),
            request => match self.renderer_request(&request, RENDERER_RENDER_TIMEOUT) {
                Ok(response) => (response, false),
                Err(message) => (Response::Error { message }.to_json(), false),
            },
        }
    }

    fn handle_wallpaper_request(&mut self, request: &WallpaperRequest) -> Response {
        let health = match self.renderer_health() {
            Ok(health) => health,
            Err(message) => return Response::Error { message },
        };
        let outputs = active_outputs_from_health(&health.outputs);
        let result = match &request.action {
            WallpaperAction::Favorites => Ok(self.wallpaper.favorites_response()),
            WallpaperAction::Current => self.wallpaper.current_response(&outputs),
            WallpaperAction::Rescan => self.wallpaper.rescan_response(),
            WallpaperAction::Favorite { index } => {
                self.wallpaper.favorite_action(&outputs, *index, true)
            }
            WallpaperAction::Unfavorite { index } => {
                self.wallpaper.favorite_action(&outputs, *index, false)
            }
            _ => self.render_wallpaper_action(request, &outputs, &health.outputs),
        };

        match result {
            Ok(response) => Response::Wallpaper(response),
            Err(message) => Response::Error { message },
        }
    }

    fn handle_world_set_request(&mut self, request: &SetRequest) -> String {
        let health = match self.renderer_health() {
            Ok(health) => health,
            Err(message) => return Response::Error { message }.to_json(),
        };
        if let Err(message) = validate_image_paths(&request.outputs) {
            return Response::Error { message }.to_json();
        }

        let entries = wallpaper_entries_for_set_outputs(&request.outputs);
        if let Err(error) = self.validate_world_cache_for_entries(&entries, &health.outputs) {
            if error.allow_fallback {
                let warmup_message =
                    self.world_route_warmup_message(&error, &entries, &health.outputs);
                if let Some(fallback) = self.config.world_fallback_transition() {
                    eprintln!(
                        "murald: explicit world set unavailable, using fallback {}: {warmup_message}",
                        transition_name(fallback),
                    );
                    let fallback_request = SetRequest {
                        outputs: request.outputs.clone(),
                        transition: fallback,
                        scale_mode: request.scale_mode,
                        allow_partial: request.allow_partial,
                    };
                    return self
                        .renderer_request(&Request::Set(fallback_request), RENDERER_RENDER_TIMEOUT)
                        .unwrap_or_else(|message| Response::Error { message }.to_json());
                }
                return Response::Error {
                    message: warmup_message,
                }
                .to_json();
            }
            return Response::Error {
                message: error.message,
            }
            .to_json();
        }

        let render_request = match self.world_render_request_for_entries(
            &entries,
            request.transition,
            request.scale_mode,
            request.allow_partial,
            request.outputs.clone(),
            &health.outputs,
        ) {
            Ok(render_request) => render_request,
            Err(message) => return Response::Error { message }.to_json(),
        };
        let response = self.renderer_request(
            &Request::RenderWorldSet(render_request),
            RENDERER_RENDER_TIMEOUT,
        );
        match response {
            Ok(response) => {
                if !response_is_error(&response) {
                    let near_future = self
                        .wallpaper
                        .upcoming_shuffle_paths(WORLD_NEAR_FUTURE_WARMUP_COUNT);
                    self.schedule_world_neighborhood_warmup_for_entries(
                        &entries,
                        &health.outputs,
                        &near_future,
                    );
                }
                response
            }
            Err(message) => Response::Error { message }.to_json(),
        }
    }

    fn world_route_warmup_message(
        &mut self,
        error: &WorldRouteCacheError,
        entries: &[WallpaperEntry],
        health_outputs: &[HealthOutput],
    ) -> String {
        let compute_guidance = world_route_compute_guidance(entries, health_outputs);
        let warmup_status = match error.auto_warmup {
            Some(estimate) => match estimate.auto_skip_reason() {
                Some(reason) => reason,
                None => self.schedule_world_route_warmup(entries, health_outputs),
            },
            None => {
                "auto route warmup skipped: route warmup cost could not be bounded; run the command manually if you want this warmup"
                    .to_owned()
            }
        };
        format!("{}; {warmup_status}; {compute_guidance}", error.message)
    }

    fn render_wallpaper_action(
        &mut self,
        request: &WallpaperRequest,
        outputs: &[ActiveOutput],
        health_outputs: &[HealthOutput],
    ) -> Result<mural_ipc::WallpaperResponse, String> {
        validate_render_wallpaper_action(&request.action, outputs)?;
        if all_active_outputs_power_off(outputs, health_outputs) {
            return Ok(skipped_wallpaper_action_response(&request.action, outputs));
        }

        let mut transition = request
            .transition
            .unwrap_or_else(|| self.config.transition_for_action(&request.action));
        let is_world_transition = matches!(transition, Transition::World { .. });
        let scale_mode = request.scale_mode.unwrap_or(self.config.scale_mode);
        let capture_canvas_positions = matches!(transition, Transition::Canvas { .. });
        let prepared = self.wallpaper.prepare_wallpaper_change(
            &request.action,
            outputs,
            capture_canvas_positions,
        )?;
        let request_outputs = prepared
            .entries
            .iter()
            .map(|entry| (entry.output.clone(), entry.path.clone()))
            .collect::<BTreeMap<_, _>>();
        validate_image_paths(&request_outputs)?;

        if is_world_transition {
            let result = self.validate_world_cache_for_prepared_change(&prepared, health_outputs);
            if let Err(error) = result {
                if error.allow_fallback {
                    let warmup_message =
                        self.world_route_warmup_message(&error, &prepared.entries, health_outputs);
                    if let Some(fallback) = self.config.world_fallback_transition() {
                        eprintln!(
                            "murald: world transition unavailable, using fallback {}: {warmup_message}",
                            transition_name(fallback),
                        );
                        transition = fallback;
                    } else {
                        self.wallpaper.rollback_wallpaper_change(prepared);
                        return Err(warmup_message);
                    }
                } else {
                    self.wallpaper.rollback_wallpaper_change(prepared);
                    return Err(error.message);
                }
            }
        }

        let mut prepared = prepared;
        self.wallpaper.move_quarantine(&mut prepared)?;
        let render_result = self.render_prepared_change(&prepared, transition, scale_mode);
        if let Err(message) = render_result {
            self.wallpaper.rollback_wallpaper_change(prepared);
            return Err(message);
        }
        let response = self.wallpaper.commit_wallpaper_change(prepared)?;
        if matches!(transition, Transition::World { .. }) {
            let near_future = self
                .wallpaper
                .upcoming_shuffle_paths(WORLD_NEAR_FUTURE_WARMUP_COUNT);
            self.schedule_world_neighborhood_warmup_for_entries(
                &response.entries,
                health_outputs,
                &near_future,
            );
        }
        Ok(response)
    }

    fn render_prepared_change(
        &mut self,
        prepared: &PreparedWallpaperChange,
        transition: Transition,
        scale_mode: mural_ipc::ScaleMode,
    ) -> Result<(), String> {
        let outputs = prepared
            .entries
            .iter()
            .map(|entry| (entry.output.clone(), entry.path.clone()))
            .collect::<BTreeMap<_, _>>();
        let response = if matches!(transition, Transition::Canvas { .. }) {
            let request = self.canvas_render_request(prepared, transition, scale_mode, outputs)?;
            self.renderer_request(&Request::RenderCanvasSet(request), RENDERER_RENDER_TIMEOUT)?
        } else if matches!(transition, Transition::World { .. }) {
            let request = self.world_render_request(prepared, transition, scale_mode, outputs)?;
            self.renderer_request(&Request::RenderWorldSet(request), RENDERER_RENDER_TIMEOUT)?
        } else {
            let request = SetRequest {
                outputs,
                transition,
                scale_mode,
                allow_partial: false,
            };
            self.renderer_request(&Request::Set(request), RENDERER_RENDER_TIMEOUT)?
        };
        if response_is_error(&response) {
            return Err(response_error_message(&response).unwrap_or(response));
        }
        Ok(())
    }

    fn validate_world_cache_for_prepared_change(
        &mut self,
        prepared: &PreparedWallpaperChange,
        health_outputs: &[HealthOutput],
    ) -> Result<(), WorldRouteCacheError> {
        self.validate_world_cache_for_entries(&prepared.entries, health_outputs)
    }

    fn validate_world_cache_for_entries(
        &mut self,
        entries: &[WallpaperEntry],
        health_outputs: &[HealthOutput],
    ) -> Result<(), WorldRouteCacheError> {
        let plan = self.world_route_cache_plan_for_entries(entries, health_outputs)?;

        let layout = WorldLayout::new(plan.snapshot.library_count(), plan.snapshot.columns);
        let tile_entries = plan
            .tile_entries
            .into_iter()
            .map(|entry| {
                (
                    (entry.lod, entry.tile_row, entry.tile_column),
                    entry.image_path,
                )
            })
            .collect::<BTreeMap<_, _>>();
        let mut missing = BTreeMap::new();

        for route in plan.routes.values() {
            let route_tiles = world_tiles_for_route(
                layout,
                route.current_index,
                route.target_index,
                world_lod_tile_cells(DEFAULT_WORLD_TILE_CELLS, route.lod),
                1.0,
            );
            validate_world_route_budget(route_tiles.len(), route.lod)
                .map_err(WorldRouteCacheError::fallback)?;

            for tile in route_tiles {
                let Some(path) = tile_entries.get(&(route.lod, tile.row, tile.column)) else {
                    missing.insert(
                        (route.lod, tile.row, tile.column),
                        format!("l{}/{:06}-{:06}.png", route.lod, tile.row, tile.column),
                    );
                    continue;
                };
                if !path.is_file() {
                    missing.insert(
                        (route.lod, tile.row, tile.column),
                        path.display().to_string(),
                    );
                }
            }
        }

        if missing.is_empty() {
            Ok(())
        } else {
            let first = missing.values().next().cloned().unwrap_or_default();
            let estimate = WorldRouteWarmupEstimate {
                tile_count: missing.len(),
                max_lod: missing
                    .keys()
                    .map(|(lod, _, _)| *lod)
                    .max()
                    .unwrap_or_default(),
            };
            Err(WorldRouteCacheError::fallback_with_warmup(
                format!(
                    "world cache is missing {} route tile(s), first missing: {first}",
                    missing.len(),
                ),
                estimate,
            ))
        }
    }

    fn schedule_world_route_warmup(
        &mut self,
        entries: &[WallpaperEntry],
        health_outputs: &[HealthOutput],
    ) -> String {
        let Some(args) = world_route_compute_args(entries, health_outputs, true) else {
            return "could not schedule route warmup because no active route start was available"
                .to_owned();
        };
        self.schedule_world_cache_warmup("route warmup", &args)
    }

    fn schedule_world_neighborhood_warmup_for_entries(
        &mut self,
        entries: &[WallpaperEntry],
        health_outputs: &[HealthOutput],
        near_future_paths: &[String],
    ) {
        let centers = world_neighborhood_centers(entries, health_outputs, near_future_paths);
        let Some(args) = world_neighborhood_compute_args(&centers, true) else {
            return;
        };
        let status = self.schedule_world_cache_warmup("neighborhood warmup", &args);
        eprintln!("murald: {status}");
    }

    fn schedule_world_cache_warmup(&mut self, label: &str, args: &[String]) -> String {
        self.prune_scheduled_world_warmups();
        let key = world_warmup_key(args);
        if self
            .scheduled_world_warmups
            .insert(key.clone(), Instant::now())
            .is_some()
        {
            return format!("{label} already scheduled for this request");
        }

        let executable = muralctl_executable();
        match Command::new(&executable)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
        {
            Ok(output) if output.status.success() => format!(
                "scheduled {label}: {}",
                background_world_cache_compute_summary(&output.stdout)
            ),
            Ok(output) => {
                self.scheduled_world_warmups.remove(&key);
                format!(
                    "failed to schedule {label} with {}: {}",
                    executable.display(),
                    process_output_summary(&output)
                )
            }
            Err(error) => {
                self.scheduled_world_warmups.remove(&key);
                format!(
                    "failed to schedule {label} with {}: {error}",
                    executable.display()
                )
            }
        }
    }

    fn prune_scheduled_world_warmups(&mut self) {
        let now = Instant::now();
        self.scheduled_world_warmups
            .retain(|_, started| now.duration_since(*started) < WORLD_ROUTE_WARMUP_DEDUPE_TTL);
    }

    fn world_route_cache_plan_for_entries(
        &self,
        entries: &[WallpaperEntry],
        health_outputs: &[HealthOutput],
    ) -> Result<WorldRouteCachePlan, WorldRouteCacheError> {
        let status = world_cache_status(&self.config).map_err(WorldRouteCacheError::hard)?;
        let snapshot = self.world_route_cache_snapshot(&status)?;
        let tile_entries = world_tile_pyramid_cache_entries_for_snapshot(
            &self.config,
            &snapshot,
            DEFAULT_WORLD_CELL_THUMBNAIL_EDGE,
            DEFAULT_WORLD_TILE_CELLS,
        )
        .map_err(WorldRouteCacheError::hard)?;
        let lods = world_lod_cache_statuses(&tile_entries);
        let world_snapshot = WorldSnapshot::new(snapshot.library.clone(), snapshot.columns);
        let layout = world_snapshot.layout();
        let mut routes = BTreeMap::new();

        for entry in entries {
            let current_path = health_outputs
                .iter()
                .find(|output| output.name == entry.output)
                .and_then(|output| {
                    output
                        .transition_target_image
                        .as_deref()
                        .or(output.current_image.as_deref())
                })
                .ok_or_else(|| {
                    format!(
                        "world transition requires a current wallpaper for output {}",
                        entry.output
                    )
                })
                .map_err(WorldRouteCacheError::fallback)?;
            let current_index = world_snapshot.index_of(current_path).ok_or_else(|| {
                format!(
                    "world transition current wallpaper is not in the indexed cache snapshot: {current_path}"
                )
            })
            .map_err(WorldRouteCacheError::fallback)?;
            let target_index = world_snapshot.index_of(&entry.path).ok_or_else(|| {
                format!(
                    "world transition target wallpaper is not in the indexed cache snapshot: {}",
                    entry.path
                )
            })
            .map_err(WorldRouteCacheError::fallback)?;
            let lod = select_world_route_lod(&lods, layout, current_index, target_index)
                .map_err(WorldRouteCacheError::fallback)?;
            routes.insert(
                entry.output.clone(),
                WorldRouteFocus {
                    current_index,
                    target_index,
                    lod,
                },
            );
        }

        Ok(WorldRouteCachePlan {
            snapshot,
            routes,
            tile_entries,
        })
    }

    fn world_route_cache_snapshot(
        &self,
        status: &WorldCacheStatus,
    ) -> Result<WorldCacheSnapshot, WorldRouteCacheError> {
        match status.manifest_state {
            ManifestState::Current => {
                let snapshot = world_cache_snapshot_for_library(self.wallpaper.library_paths());
                if snapshot.library_count() == status.library_count
                    && snapshot.columns == status.columns
                    && snapshot.rows == status.rows
                    && snapshot.fingerprint == status.fingerprint
                {
                    Ok(snapshot)
                } else {
                    Err(WorldRouteCacheError::fallback(
                        "world cache status does not match the in-memory wallpaper snapshot"
                            .to_owned(),
                    ))
                }
            }
            ManifestState::Stale => read_indexed_world_cache_snapshot(&self.config)
                .map_err(WorldRouteCacheError::hard)?
                .ok_or_else(|| {
                    WorldRouteCacheError::fallback(
                        "world cache manifest is stale and its indexed path snapshot is unavailable; run `muralctl world cache compute --scope all --background --progress`"
                            .to_owned(),
                    )
                }),
            ManifestState::Missing | ManifestState::Invalid => Err(WorldRouteCacheError::hard(
                format!("world cache is not ready: {}", status.message),
            )),
        }
    }
    fn canvas_render_request(
        &mut self,
        prepared: &PreparedWallpaperChange,
        transition: Transition,
        scale_mode: mural_ipc::ScaleMode,
        outputs: BTreeMap<String, String>,
    ) -> Result<RenderCanvasSetRequest, String> {
        let Transition::Canvas {
            pan_axis,
            overview_scale,
            tile_count,
            ..
        } = transition
        else {
            return Err("canvas render request requires a canvas transition".to_owned());
        };
        let health = self.renderer_health()?;
        let current = prepared
            .entries
            .iter()
            .filter_map(|entry| {
                health
                    .outputs
                    .iter()
                    .find(|output| output.name == entry.output)
                    .and_then(|output| output.current_image.clone())
            })
            .collect::<Vec<_>>();
        let tile_count =
            Self::resolve_canvas_tile_count(&health.outputs, tile_count, overview_scale, pan_axis);
        let preview = self
            .wallpaper
            .canvas_preview_window_for_prepared_change(prepared, &current, tile_count)?;

        Ok(RenderCanvasSetRequest {
            outputs,
            transition,
            scale_mode,
            allow_partial: false,
            preview_paths: preview.paths,
            preview_start: preview.start_index,
        })
    }

    fn world_render_request(
        &mut self,
        prepared: &PreparedWallpaperChange,
        transition: Transition,
        scale_mode: mural_ipc::ScaleMode,
        outputs: BTreeMap<String, String>,
    ) -> Result<RenderWorldSetRequest, String> {
        let health = self.renderer_health()?;
        self.world_render_request_for_entries(
            &prepared.entries,
            transition,
            scale_mode,
            false,
            outputs,
            &health.outputs,
        )
    }

    fn world_render_request_for_entries(
        &self,
        entries: &[WallpaperEntry],
        transition: Transition,
        scale_mode: mural_ipc::ScaleMode,
        allow_partial: bool,
        outputs: BTreeMap<String, String>,
        health_outputs: &[HealthOutput],
    ) -> Result<RenderWorldSetRequest, String> {
        if !matches!(transition, Transition::World { .. }) {
            return Err("world render request requires a world transition".to_owned());
        }

        let plan = self
            .world_route_cache_plan_for_entries(entries, health_outputs)
            .map_err(|error| error.message)?;

        Ok(RenderWorldSetRequest {
            outputs,
            transition,
            scale_mode,
            allow_partial,
            library_count: plan.snapshot.library_count(),
            columns: plan.snapshot.columns,
            fingerprint: plan.snapshot.fingerprint,
            thumbnail_edge: DEFAULT_WORLD_CELL_THUMBNAIL_EDGE,
            tile_cells: DEFAULT_WORLD_TILE_CELLS,
            routes: plan.routes,
        })
    }

    fn resolve_canvas_tile_count(
        outputs: &[HealthOutput],
        tile_count: mural_ipc::CanvasTileCount,
        overview_scale: f32,
        pan_axis: CanvasPanAxis,
    ) -> usize {
        let output = outputs
            .iter()
            .filter_map(|output| {
                let width = u32::try_from(output.width).ok()?;
                let height = u32::try_from(output.height).ok()?;
                if width == 0 || height == 0 {
                    return None;
                }
                Some(Size { width, height })
            })
            .max_by_key(|size| u64::from(size.width) * u64::from(size.height))
            .unwrap_or(Size {
                width: 1920,
                height: 1080,
            });
        resolve_canvas_tile_count_for_pan(
            tile_count,
            overview_scale,
            output,
            outputs.len().max(1),
            pan_axis,
        )
    }

    fn restore_current_wallpapers(&mut self, reason: &str) {
        let outputs = match self.active_outputs() {
            Ok(outputs) => outputs,
            Err(message) => {
                eprintln!("murald: {reason} restore skipped: {message}");
                return;
            }
        };
        if outputs.is_empty() {
            return;
        }
        let prepared = match self.wallpaper.prepare_startup_display(&outputs) {
            Ok(prepared) => prepared,
            Err(message) => {
                eprintln!("murald: {reason} restore skipped: {message}");
                return;
            }
        };
        let scale_mode = self.config.scale_mode;
        match self.render_prepared_change(&prepared, Transition::Cut, scale_mode) {
            Ok(()) => match self.wallpaper.commit_wallpaper_change(prepared) {
                Ok(response) => eprintln!(
                    "murald: {reason} restored {} wallpaper(s)",
                    response.entries.len()
                ),
                Err(message) => eprintln!("murald: {reason} restore commit failed: {message}"),
            },
            Err(message) => eprintln!("murald: {reason} restore render failed: {message}"),
        }
    }

    fn maybe_restore_pending_renderer_surfaces(&mut self) {
        if self.last_restore_check.elapsed() < RESTORE_PENDING_CHECK_INTERVAL {
            return;
        }
        self.last_restore_check = Instant::now();

        let health = match self.renderer_health() {
            Ok(health) => health,
            Err(message) => {
                eprintln!("murald: renderer surface restore check skipped: {message}");
                return;
            }
        };
        let targets = health
            .outputs
            .iter()
            .filter(|output| output.restore_pending && output.render_state == "renderable")
            .map(|output| output.name.clone())
            .collect::<BTreeSet<_>>();
        if targets.is_empty() {
            return;
        }

        self.restore_renderer_surfaces_from_health(&health.outputs, &targets);
    }

    fn restore_renderer_surfaces_from_health(
        &mut self,
        outputs: &[HealthOutput],
        targets: &BTreeSet<String>,
    ) {
        let active_outputs = active_outputs_from_health(outputs);
        if active_outputs.is_empty() {
            return;
        }

        let response = match self.wallpaper.current_response(&active_outputs) {
            Ok(response) => response,
            Err(message) => {
                eprintln!(
                    "murald: renderer surface restore falling back to startup selection: {message}"
                );
                self.restore_current_wallpapers("renderer surface restore");
                return;
            }
        };
        let render_outputs = response
            .entries
            .into_iter()
            .filter(|entry| targets.contains(&entry.output))
            .map(|entry| (entry.output, entry.path))
            .collect::<BTreeMap<_, _>>();
        if render_outputs.is_empty() {
            return;
        }

        let names = render_outputs
            .keys()
            .cloned()
            .collect::<Vec<_>>()
            .join(", ");
        match self.render_cut_outputs(render_outputs, self.config.scale_mode) {
            Ok(()) => eprintln!("murald: restored renderer surface(s): {names}"),
            Err(message) => eprintln!("murald: renderer surface restore failed: {message}"),
        }
    }

    fn render_cut_outputs(
        &mut self,
        outputs: BTreeMap<String, String>,
        scale_mode: mural_ipc::ScaleMode,
    ) -> Result<(), String> {
        validate_image_paths(&outputs)?;
        let request = SetRequest {
            outputs,
            transition: Transition::Cut,
            scale_mode,
            allow_partial: false,
        };
        let response = self.renderer_request(&Request::Set(request), RENDERER_RENDER_TIMEOUT)?;
        if response_is_error(&response) {
            return Err(response_error_message(&response).unwrap_or(response));
        }
        Ok(())
    }

    fn active_outputs(&mut self) -> Result<Vec<ActiveOutput>, String> {
        let health = self.renderer_health()?;
        Ok(active_outputs_from_health(&health.outputs))
    }

    fn health_response(&mut self) -> HealthResponse {
        let child_health = self.renderer_health().ok();
        HealthResponse {
            role: "supervisor".to_owned(),
            supervisor_pid: Some(std::process::id()),
            renderer_pid: self.renderer_pid(),
            renderer_generation: self
                .renderer
                .as_ref()
                .map_or(0, |renderer| renderer.generation),
            renderer_state: child_health.as_ref().map_or_else(
                || "unavailable".to_owned(),
                |health| health.renderer_state.clone(),
            ),
            restart_count: self.restart_count,
            last_error: self.last_error.clone(),
            last_diagnostic: self.last_diagnostic.clone(),
            outputs: child_health.map_or_else(Vec::new, |health| health.outputs),
        }
    }

    fn renderer_health(&mut self) -> Result<HealthResponse, String> {
        let response = self.renderer_request(&Request::Health, RENDERER_HEALTH_TIMEOUT)?;
        parse_health_response(&response).map_err(|error| error.to_string())
    }

    fn renderer_health_no_restart(&mut self) -> Result<HealthResponse, String> {
        let response = self
            .renderer
            .as_mut()
            .ok_or_else(|| "renderer is unavailable".to_owned())?
            .request(&Request::Health, RENDERER_HEALTH_TIMEOUT)?;
        parse_health_response(&response).map_err(|error| error.to_string())
    }

    fn renderer_request(&mut self, request: &Request, timeout: Duration) -> Result<String, String> {
        self.ensure_renderer()?;
        let result = self
            .renderer
            .as_mut()
            .ok_or_else(|| "renderer is unavailable".to_owned())?
            .request(request, timeout);
        match result {
            Ok(response) => Ok(response),
            Err(error) => {
                let reason = format!("renderer request failed: {error}");
                self.restart_renderer(&reason)?;
                Err(reason)
            }
        }
    }

    fn ensure_renderer(&mut self) -> Result<(), String> {
        let exited = match self.renderer.as_mut() {
            Some(renderer) => renderer.exited()?,
            None => true,
        };
        if exited {
            self.restart_renderer("renderer exited")?;
        }
        Ok(())
    }

    fn spawn_renderer(&mut self) -> Result<(), String> {
        let generation = self.next_generation;
        self.next_generation = self.next_generation.saturating_add(1);
        let renderer = RendererProcess::spawn(generation, self.trace)?;
        eprintln!(
            "murald: renderer generation {generation} started with pid {}",
            renderer.child.id()
        );
        self.renderer = Some(renderer);
        Ok(())
    }

    fn restart_renderer(&mut self, reason: &str) -> Result<(), String> {
        self.last_error = Some(reason.to_owned());
        if let Some(mut renderer) = self.renderer.take() {
            self.last_diagnostic = renderer.capture_diagnostic(reason, self.wallpaper.state_dir());
            renderer.abort_then_kill();
        }
        self.restart_count = self.restart_count.saturating_add(1);
        self.spawn_renderer()?;
        self.wait_renderer_ready("renderer restart");
        self.restore_current_wallpapers("renderer restart");
        Ok(())
    }

    fn stop_renderer(&mut self) {
        if let Some(mut renderer) = self.renderer.take() {
            let _ = renderer.request(&Request::Stop, RENDERER_EXIT_TIMEOUT);
            renderer.kill_if_still_running();
        }
    }

    fn wait_renderer_ready(&mut self, reason: &str) {
        let deadline = Instant::now() + RENDERER_READY_TIMEOUT;
        loop {
            match self.renderer_health_no_restart() {
                Ok(health) if renderer_outputs_ready(&health.outputs) => return,
                Ok(_health) if Instant::now() < deadline => {
                    thread::sleep(Duration::from_millis(100));
                }
                Ok(_health) => {
                    eprintln!("murald: {reason} renderer did not become renderable before timeout");
                    return;
                }
                Err(error) => {
                    eprintln!("murald: {reason} renderer readiness check failed: {error}");
                    return;
                }
            }
        }
    }

    fn renderer_pid(&self) -> Option<u32> {
        self.renderer.as_ref().map(|renderer| renderer.child.id())
    }

    fn next_ipc_id(&mut self) -> u64 {
        let id = self.next_ipc_id;
        self.next_ipc_id = self.next_ipc_id.wrapping_add(1).max(1);
        id
    }
}

fn select_world_route_lod(
    lods: &[WorldLodCacheStatus],
    layout: WorldLayout,
    current_index: usize,
    target_index: usize,
) -> Result<usize, String> {
    let candidates = lods
        .iter()
        .map(|lod| WorldRouteLodCandidate {
            lod: lod.lod,
            tile_cells: world_lod_tile_cells(DEFAULT_WORLD_TILE_CELLS, lod.lod),
        })
        .collect::<Vec<_>>();
    world_route_lod_for_budget(
        layout,
        current_index,
        target_index,
        &candidates,
        MAX_WORLD_ROUTE_TILES,
        1.0,
    )
    .map(|selection| selection.lod)
    .ok_or_else(|| {
        format!(
            "world transition route exceeds the current renderer tile budget of {MAX_WORLD_ROUTE_TILES} at every available cache LOD"
        )
    })
}

fn validate_world_route_budget(route_tile_count: usize, lod: usize) -> Result<(), String> {
    if route_tile_count <= MAX_WORLD_ROUTE_TILES {
        return Ok(());
    }

    Err(format!(
        "world transition route needs {route_tile_count} tile(s) at LOD {lod}, exceeding the current safe renderer limit of {MAX_WORLD_ROUTE_TILES}"
    ))
}

fn wallpaper_entries_for_set_outputs(outputs: &BTreeMap<String, String>) -> Vec<WallpaperEntry> {
    outputs
        .iter()
        .enumerate()
        .map(|(index, (output, path))| WallpaperEntry {
            index,
            output: output.clone(),
            favorite: false,
            path: path.clone(),
        })
        .collect()
}

fn world_route_compute_guidance(
    entries: &[WallpaperEntry],
    health_outputs: &[HealthOutput],
) -> String {
    let Some(command) = world_route_compute_command(entries, health_outputs) else {
        return "run `muralctl world cache compute --scope all --background --progress`".to_owned();
    };

    format!(
        "run route warmup: `{command}`; add `--background` to detach it, or warm the full cache with `muralctl world cache compute --scope all --background --progress`"
    )
}

fn world_route_compute_command(
    entries: &[WallpaperEntry],
    health_outputs: &[HealthOutput],
) -> Option<String> {
    let args = world_route_compute_args(entries, health_outputs, false)?;
    let command_args = args
        .iter()
        .map(|arg| shell_quote(arg))
        .collect::<Vec<_>>()
        .join(" ");
    Some(format!("muralctl {command_args}"))
}

fn world_route_compute_args(
    entries: &[WallpaperEntry],
    health_outputs: &[HealthOutput],
    background: bool,
) -> Option<Vec<String>> {
    let routes = entries
        .iter()
        .filter_map(|entry| {
            let current_path = health_outputs
                .iter()
                .find(|output| output.name == entry.output)
                .and_then(world_route_start_path)?;
            Some((current_path.to_owned(), entry.path.clone()))
        })
        .collect::<Vec<_>>();
    if routes.is_empty() {
        return None;
    }

    let mut args = vec![
        "world".to_owned(),
        "cache".to_owned(),
        "compute".to_owned(),
        "--scope".to_owned(),
        "route".to_owned(),
    ];
    for (current_path, target_path) in routes {
        args.push("--route".to_owned());
        args.push(current_path);
        args.push(target_path);
    }
    if background {
        args.push("--background".to_owned());
    }
    args.push("--progress".to_owned());
    Some(args)
}

fn world_neighborhood_centers(
    entries: &[WallpaperEntry],
    health_outputs: &[HealthOutput],
    near_future_paths: &[String],
) -> Vec<String> {
    let mut centers = Vec::new();
    for entry in entries {
        if let Some(current_path) = health_outputs
            .iter()
            .find(|output| output.name == entry.output)
            .and_then(world_route_start_path)
        {
            push_unique_string(&mut centers, current_path.to_owned());
        }
        push_unique_string(&mut centers, entry.path.clone());
    }
    for path in near_future_paths {
        push_unique_string(&mut centers, path.clone());
    }
    centers
}

fn world_neighborhood_compute_args(centers: &[String], background: bool) -> Option<Vec<String>> {
    if centers.is_empty() {
        return None;
    }

    let mut args = vec![
        "world".to_owned(),
        "cache".to_owned(),
        "compute".to_owned(),
        "--scope".to_owned(),
        "neighborhood".to_owned(),
    ];
    for center in centers {
        args.push("--center".to_owned());
        args.push(center.clone());
    }
    args.extend([
        "--radius".to_owned(),
        "0".to_owned(),
        "--lod".to_owned(),
        "0".to_owned(),
        "--tile-limit".to_owned(),
        WORLD_NEIGHBORHOOD_WARMUP_TILE_LIMIT.to_string(),
    ]);
    if background {
        args.push("--background".to_owned());
    }
    args.push("--progress".to_owned());
    Some(args)
}

fn push_unique_string(values: &mut Vec<String>, value: String) {
    if !values.iter().any(|candidate| candidate == &value) {
        values.push(value);
    }
}

fn world_warmup_key(args: &[String]) -> String {
    args.join("\0")
}

fn muralctl_executable() -> PathBuf {
    std::env::current_exe()
        .ok()
        .map(|path| path.with_file_name("muralctl"))
        .filter(|path| path.is_file())
        .unwrap_or_else(|| PathBuf::from("muralctl"))
}

fn background_world_cache_compute_summary(stdout: &[u8]) -> String {
    let output = String::from_utf8_lossy(stdout);
    let mut background_pid = None;
    let mut background_log = None;
    for line in output.lines() {
        if let Some(value) = line.strip_prefix("background_pid\t") {
            background_pid = Some(value.to_owned());
        } else if let Some(value) = line.strip_prefix("background_log\t") {
            background_log = Some(value.to_owned());
        }
    }

    match (background_pid, background_log) {
        (Some(pid), Some(log)) => {
            format!("background_pid={pid}, background_log={log}; retry after it finishes")
        }
        (Some(pid), None) => format!("background_pid={pid}; retry after it finishes"),
        (None, Some(log)) => format!("background_log={log}; retry after it finishes"),
        (None, None) => {
            let summary = compact_output(stdout);
            if summary.is_empty() {
                "started; retry after it finishes".to_owned()
            } else {
                format!("{summary}; retry after it finishes")
            }
        }
    }
}

fn process_output_summary(output: &std::process::Output) -> String {
    let mut parts = vec![format!("exit status {}", output.status)];
    let stdout = compact_output(&output.stdout);
    if !stdout.is_empty() {
        parts.push(format!("stdout: {stdout}"));
    }
    let stderr = compact_output(&output.stderr);
    if !stderr.is_empty() {
        parts.push(format!("stderr: {stderr}"));
    }
    parts.join("; ")
}

fn compact_output(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .take(4)
        .collect::<Vec<_>>()
        .join("; ")
}

fn world_route_start_path(output: &HealthOutput) -> Option<&str> {
    output
        .transition_target_image
        .as_deref()
        .or(output.current_image.as_deref())
}

fn shell_quote(value: &str) -> String {
    if value
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || "/._-".contains(character))
    {
        return value.to_owned();
    }

    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

struct RendererProcess {
    child: Child,
    stream: UnixStream,
    generation: u64,
}

impl RendererProcess {
    fn spawn(generation: u64, trace: TraceMode) -> Result<Self, String> {
        let (parent_stream, child_stream) = UnixStream::pair()
            .map_err(|error| format!("failed to create renderer control socketpair: {error}"))?;
        let child_fd = child_stream.as_raw_fd();
        set_close_on_exec(child_fd, false)?;

        let exe = std::env::current_exe()
            .map_err(|error| format!("failed to locate murald executable: {error}"))?;
        let mut command = Command::new(exe);
        command
            .arg("--renderer-child")
            .arg("--renderer-fd")
            .arg(child_fd.to_string())
            .env_remove("NOTIFY_SOCKET")
            .env_remove("WATCHDOG_USEC")
            .env_remove("WATCHDOG_PID");
        if trace.enabled() {
            command.arg("--trace");
        }
        let child = command
            .spawn()
            .map_err(|error| format!("failed to spawn renderer child: {error}"))?;
        drop(child_stream);

        parent_stream
            .set_read_timeout(Some(RENDERER_READY_TIMEOUT))
            .map_err(|error| format!("failed to set renderer ready timeout: {error}"))?;
        match read_frame(&parent_stream)? {
            Some(response) if !response_is_error(&response) => {}
            Some(response) => {
                return Err(response_error_message(&response).unwrap_or(response));
            }
            None => return Err("renderer exited before reporting ready".to_owned()),
        }
        parent_stream
            .set_read_timeout(None)
            .map_err(|error| format!("failed to clear renderer ready timeout: {error}"))?;

        Ok(Self {
            child,
            stream: parent_stream,
            generation,
        })
    }

    fn request(&mut self, request: &Request, timeout: Duration) -> Result<String, String> {
        self.stream
            .set_read_timeout(Some(timeout))
            .map_err(|error| format!("failed to set renderer read timeout: {error}"))?;
        self.stream
            .set_write_timeout(Some(timeout))
            .map_err(|error| format!("failed to set renderer write timeout: {error}"))?;
        write_frame(&self.stream, &request.to_json())?;
        read_frame(&self.stream)?.ok_or_else(|| "renderer control channel closed".to_owned())
    }

    fn exited(&mut self) -> Result<bool, String> {
        self.child
            .try_wait()
            .map(|status| status.is_some())
            .map_err(|error| format!("failed to poll renderer child: {error}"))
    }

    fn capture_diagnostic(&mut self, reason: &str, state_dir: &Path) -> Option<String> {
        let diagnostics_dir = state_dir.join("diagnostics");
        if let Err(error) = fs::create_dir_all(&diagnostics_dir) {
            eprintln!("murald: failed to create diagnostics directory: {error}");
            return None;
        }
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_secs());
        let path = diagnostics_dir.join(format!("renderer-{}-{}.txt", self.child.id(), timestamp));
        let mut body = format!(
            "reason: {reason}\nrenderer_pid: {}\ngeneration: {}\n\n",
            self.child.id(),
            self.generation
        );
        match run_gdb_backtrace(self.child.id(), DIAGNOSTIC_TIMEOUT) {
            Ok(output) => body.push_str(&output),
            Err(error) => {
                let _ = writeln!(body, "gdb diagnostic failed: {error}");
            }
        }
        match fs::write(&path, body) {
            Ok(()) => Some(path.display().to_string()),
            Err(error) => {
                eprintln!("murald: failed to write renderer diagnostic: {error}");
                None
            }
        }
    }

    fn abort_then_kill(&mut self) {
        let pid = match i32::try_from(self.child.id()) {
            Ok(pid) => pid,
            Err(error) => {
                eprintln!("murald: renderer pid does not fit pid_t: {error}");
                return;
            }
        };
        let _ = unsafe { libc::kill(pid, libc::SIGABRT) };
        let deadline = Instant::now() + RENDERER_EXIT_TIMEOUT;
        while Instant::now() < deadline {
            match self.child.try_wait() {
                Ok(Some(_status)) => return,
                Ok(None) => thread::sleep(Duration::from_millis(50)),
                Err(error) => {
                    eprintln!("murald: failed to poll renderer after SIGABRT: {error}");
                    break;
                }
            }
        }
        if let Err(error) = self.child.kill() {
            eprintln!("murald: failed to kill renderer child: {error}");
        }
        let _ = self.child.wait();
    }

    fn kill_if_still_running(&mut self) {
        let deadline = Instant::now() + RENDERER_EXIT_TIMEOUT;
        while Instant::now() < deadline {
            match self.child.try_wait() {
                Ok(Some(_status)) => return,
                Ok(None) => thread::sleep(Duration::from_millis(50)),
                Err(error) => {
                    eprintln!("murald: failed to poll renderer during shutdown: {error}");
                    break;
                }
            }
        }
        self.abort_then_kill();
    }
}

fn renderer_outputs_ready(outputs: &[HealthOutput]) -> bool {
    outputs.is_empty()
        || outputs
            .iter()
            .all(|output| output.render_state == "renderable")
}

fn active_outputs_from_health(outputs: &[HealthOutput]) -> Vec<ActiveOutput> {
    let mut outputs = outputs
        .iter()
        .map(|output| ActiveOutput {
            name: output.name.clone(),
            x: output.layout_x,
            y: output.layout_y,
        })
        .collect::<Vec<_>>();
    outputs.sort_by(|left, right| {
        (left.x, left.y, left.name.as_str()).cmp(&(right.x, right.y, right.name.as_str()))
    });
    outputs
}

fn validate_render_wallpaper_action(
    action: &WallpaperAction,
    outputs: &[ActiveOutput],
) -> Result<(), String> {
    let (label, index) = match action {
        WallpaperAction::Replace { index } => ("replace", *index),
        WallpaperAction::Quarantine { index } => ("quarantine", *index),
        _ => return Ok(()),
    };
    if index >= outputs.len() {
        return Err(format!(
            "{label} index out of range (0..{})",
            outputs.len().saturating_sub(1)
        ));
    }
    Ok(())
}

fn all_active_outputs_power_off(
    active_outputs: &[ActiveOutput],
    health_outputs: &[HealthOutput],
) -> bool {
    !active_outputs.is_empty()
        && active_outputs.iter().all(|active| {
            health_outputs
                .iter()
                .any(|health| health.name == active.name && health.power_state == "off")
        })
}

fn skipped_wallpaper_action_response(
    action: &WallpaperAction,
    outputs: &[ActiveOutput],
) -> WallpaperResponse {
    let names = outputs
        .iter()
        .map(|output| output.name.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    WallpaperResponse {
        action: action.as_str().to_owned(),
        message: format!(
            "skipped {}; all target outputs are off: {names}",
            action.as_str()
        ),
        entries: Vec::new(),
        favorites: Vec::new(),
    }
}

fn run_gdb_backtrace(pid: u32, timeout: Duration) -> Result<String, String> {
    let mut child = Command::new("gdb")
        .arg("-batch")
        .arg("-p")
        .arg(pid.to_string())
        .arg("-ex")
        .arg("thread apply all bt 20")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("failed to spawn gdb: {error}"))?;
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_status)) => break,
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(50)),
            Ok(None) => {
                let _ = child.kill();
                break;
            }
            Err(error) => return Err(format!("failed to poll gdb: {error}")),
        }
    }
    let output = child
        .wait_with_output()
        .map_err(|error| format!("failed to collect gdb output: {error}"))?;
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    if !output.stderr.is_empty() {
        text.push_str("\n--- gdb stderr ---\n");
        text.push_str(&String::from_utf8_lossy(&output.stderr));
    }
    Ok(text)
}

fn set_close_on_exec(fd: i32, close_on_exec: bool) -> Result<(), String> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0 {
        return Err(format!(
            "fcntl(F_GETFD) failed: {}",
            std::io::Error::last_os_error()
        ));
    }
    let updated = if close_on_exec {
        flags | libc::FD_CLOEXEC
    } else {
        flags & !libc::FD_CLOEXEC
    };
    if unsafe { libc::fcntl(fd, libc::F_SETFD, updated) } < 0 {
        return Err(format!(
            "fcntl(F_SETFD) failed: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn active(name: &str) -> ActiveOutput {
        ActiveOutput {
            name: name.to_owned(),
            x: 0,
            y: 0,
        }
    }

    fn health(name: &str, power_state: &str) -> HealthOutput {
        HealthOutput {
            name: name.to_owned(),
            layout_x: 0,
            layout_y: 0,
            width: 1920,
            height: 1080,
            power_state: power_state.to_owned(),
            render_state: if power_state == "off" {
                "power-off".to_owned()
            } else {
                "renderable".to_owned()
            },
            restore_pending: false,
            current_image: None,
            transition_target_image: None,
            scale_mode: mural_ipc::ScaleMode::Fill,
            transition_state: mural_ipc::TransitionState::Idle,
            queue_depth: 0,
            frame_callback_pending: false,
            render_pending: false,
        }
    }

    fn health_with_current(name: &str, current_image: &str) -> HealthOutput {
        HealthOutput {
            current_image: Some(current_image.to_owned()),
            ..health(name, "on")
        }
    }

    fn health_with_active_target(
        name: &str,
        current_image: &str,
        transition_target_image: &str,
    ) -> HealthOutput {
        HealthOutput {
            transition_target_image: Some(transition_target_image.to_owned()),
            ..health_with_current(name, current_image)
        }
    }

    #[test]
    fn all_active_outputs_power_off_requires_every_active_output_off() {
        let outputs = vec![active("DP-1"), active("DP-2")];
        assert!(all_active_outputs_power_off(
            &outputs,
            &[health("DP-1", "off"), health("DP-2", "off")]
        ));
        assert!(!all_active_outputs_power_off(
            &outputs,
            &[health("DP-1", "off"), health("DP-2", "on")]
        ));
        assert!(!all_active_outputs_power_off(&[], &[health("DP-1", "off")]));
    }

    #[test]
    fn skipped_wallpaper_action_response_is_success_payload() {
        let response = skipped_wallpaper_action_response(
            &WallpaperAction::Next,
            &[active("DP-1"), active("DP-2")],
        );
        assert_eq!(response.action, "next");
        assert!(response.entries.is_empty());
        assert!(response.message.contains("all target outputs are off"));
        assert!(response.message.contains("DP-1, DP-2"));
    }

    #[test]
    fn render_action_validation_preserves_index_errors() {
        let outputs = vec![active("DP-1")];
        assert_eq!(
            validate_render_wallpaper_action(&WallpaperAction::Replace { index: 3 }, &outputs),
            Err("replace index out of range (0..0)".to_owned())
        );
    }

    #[test]
    fn world_route_budget_rejects_unbounded_routes() {
        assert!(validate_world_route_budget(MAX_WORLD_ROUTE_TILES, 0).is_ok());

        let error = validate_world_route_budget(MAX_WORLD_ROUTE_TILES + 1, 2).unwrap_err();
        assert!(error.contains("exceeding the current safe renderer limit"));
        assert!(error.contains("LOD 2"));
    }

    #[test]
    fn auto_world_route_warmup_allows_small_lod0_work() {
        let estimate = WorldRouteWarmupEstimate {
            tile_count: MAX_AUTO_WORLD_ROUTE_WARMUP_TILES,
            max_lod: MAX_AUTO_WORLD_ROUTE_WARMUP_LOD,
        };

        assert_eq!(estimate.auto_skip_reason(), None);
    }

    #[test]
    fn auto_world_route_warmup_skips_large_tile_counts() {
        let estimate = WorldRouteWarmupEstimate {
            tile_count: MAX_AUTO_WORLD_ROUTE_WARMUP_TILES + 1,
            max_lod: MAX_AUTO_WORLD_ROUTE_WARMUP_LOD,
        };
        let reason = estimate.auto_skip_reason().unwrap();

        assert!(reason.contains("auto route warmup skipped"));
        assert!(reason.contains("automatic limit"));
    }

    #[test]
    fn auto_world_route_warmup_skips_higher_lods() {
        let estimate = WorldRouteWarmupEstimate {
            tile_count: 1,
            max_lod: MAX_AUTO_WORLD_ROUTE_WARMUP_LOD + 1,
        };
        let reason = estimate.auto_skip_reason().unwrap();

        assert!(reason.contains("selected LOD 1"));
        assert!(reason.contains("automatic warmup limit LOD 0"));
    }

    #[test]
    fn world_route_lod_prefers_first_budgeted_lod() {
        let status = WorldCacheStatus {
            wall_dir: PathBuf::new(),
            state_dir: PathBuf::new(),
            cache_dir: PathBuf::new(),
            manifest_path: PathBuf::new(),
            library_count: 100_000,
            columns: 400,
            rows: 250,
            fingerprint: 0,
            order_policy: mural_core::world_cache::WORLD_ORDER_POLICY.to_owned(),
            thumbnail_edge: DEFAULT_WORLD_CELL_THUMBNAIL_EDGE,
            cell_ready: 0,
            cell_missing: 0,
            world_tile_ready: 0,
            world_tile_missing: 0,
            world_lods: vec![
                mural_core::world_cache::WorldLodCacheStatus {
                    lod: 0,
                    tile_ready: 0,
                    tile_missing: 0,
                },
                mural_core::world_cache::WorldLodCacheStatus {
                    lod: 1,
                    tile_ready: 0,
                    tile_missing: 0,
                },
                mural_core::world_cache::WorldLodCacheStatus {
                    lod: 2,
                    tile_ready: 0,
                    tile_missing: 0,
                },
            ],
            manifest_state: ManifestState::Current,
            ready: false,
            message: String::new(),
        };
        let layout = WorldLayout::new(status.library_count, status.columns);

        assert_eq!(
            select_world_route_lod(&status.world_lods, layout, 0, 40_100),
            Ok(1)
        );
    }

    #[test]
    fn shell_quote_preserves_safe_paths_and_quotes_spaces() {
        assert_eq!(shell_quote("/tmp/wall-01.jpg"), "/tmp/wall-01.jpg");
        assert_eq!(
            shell_quote("/tmp/wall paper's.jpg"),
            "'/tmp/wall paper'\"'\"'s.jpg'"
        );
    }

    #[test]
    fn world_route_compute_command_batches_each_output_route() {
        let entries = vec![
            WallpaperEntry {
                index: 0,
                output: "DP-1".to_owned(),
                favorite: false,
                path: "/wall/target a.jpg".to_owned(),
            },
            WallpaperEntry {
                index: 1,
                output: "DP-2".to_owned(),
                favorite: false,
                path: "/wall/target-b.jpg".to_owned(),
            },
        ];
        let health = vec![
            health_with_current("DP-1", "/wall/current a.jpg"),
            health_with_current("DP-2", "/wall/current-b.jpg"),
        ];

        let command = world_route_compute_command(&entries, &health).unwrap();

        assert!(command.contains("--route '/wall/current a.jpg' '/wall/target a.jpg'"));
        assert!(command.contains("--route /wall/current-b.jpg /wall/target-b.jpg"));
        assert!(command.ends_with("--progress"));
    }

    #[test]
    fn world_route_compute_command_uses_active_target_as_queue_start() {
        let entries = vec![WallpaperEntry {
            index: 0,
            output: "DP-1".to_owned(),
            favorite: false,
            path: "/wall/queued-target.jpg".to_owned(),
        }];
        let health = vec![health_with_active_target(
            "DP-1",
            "/wall/current.jpg",
            "/wall/active-target.jpg",
        )];

        let command = world_route_compute_command(&entries, &health).unwrap();

        assert!(command.contains("--route /wall/active-target.jpg /wall/queued-target.jpg"));
        assert!(!command.contains("/wall/current.jpg"));
    }

    #[test]
    fn world_route_compute_args_can_detach_background_warmups() {
        let entries = vec![WallpaperEntry {
            index: 0,
            output: "DP-1".to_owned(),
            favorite: false,
            path: "/wall/target.jpg".to_owned(),
        }];
        let health = vec![health_with_current("DP-1", "/wall/current.jpg")];

        let args = world_route_compute_args(&entries, &health, true).unwrap();

        assert_eq!(
            args,
            vec![
                "world",
                "cache",
                "compute",
                "--scope",
                "route",
                "--route",
                "/wall/current.jpg",
                "/wall/target.jpg",
                "--background",
                "--progress",
            ]
        );
    }

    #[test]
    fn world_neighborhood_centers_include_current_target_and_future_once() {
        let entries = vec![
            WallpaperEntry {
                index: 0,
                output: "DP-1".to_owned(),
                favorite: false,
                path: "/wall/target.jpg".to_owned(),
            },
            WallpaperEntry {
                index: 1,
                output: "DP-2".to_owned(),
                favorite: false,
                path: "/wall/current.jpg".to_owned(),
            },
        ];
        let health = vec![
            health_with_current("DP-1", "/wall/current.jpg"),
            health_with_current("DP-2", "/wall/current.jpg"),
        ];
        let near_future = vec!["/wall/future.jpg".to_owned(), "/wall/target.jpg".to_owned()];

        let centers = world_neighborhood_centers(&entries, &health, &near_future);

        assert_eq!(
            centers,
            vec![
                "/wall/current.jpg".to_owned(),
                "/wall/target.jpg".to_owned(),
                "/wall/future.jpg".to_owned(),
            ]
        );
    }

    #[test]
    fn world_neighborhood_compute_args_can_detach_prefetches() {
        let centers = vec![
            "/wall/current.jpg".to_owned(),
            "/wall/target.jpg".to_owned(),
        ];

        let args = world_neighborhood_compute_args(&centers, true).unwrap();

        assert_eq!(
            args,
            vec![
                "world",
                "cache",
                "compute",
                "--scope",
                "neighborhood",
                "--center",
                "/wall/current.jpg",
                "--center",
                "/wall/target.jpg",
                "--radius",
                "0",
                "--lod",
                "0",
                "--tile-limit",
                "4",
                "--background",
                "--progress",
            ]
        );
    }

    #[test]
    fn background_world_cache_compute_summary_reports_retry_context() {
        let stdout = b"background_pid\t1234\nbackground_log\t/tmp/world.log\nmessage\tworld cache compute started in background\n";

        let summary = background_world_cache_compute_summary(stdout);

        assert!(summary.contains("background_pid=1234"));
        assert!(summary.contains("background_log=/tmp/world.log"));
        assert!(summary.contains("retry after it finishes"));
    }

    #[test]
    fn world_route_compute_guidance_advertises_background_warmups() {
        let entries = vec![WallpaperEntry {
            index: 0,
            output: "DP-1".to_owned(),
            favorite: false,
            path: "/wall/target.jpg".to_owned(),
        }];
        let health = vec![health_with_current("DP-1", "/wall/current.jpg")];

        let guidance = world_route_compute_guidance(&entries, &health);
        let fallback = world_route_compute_guidance(&entries, &[]);

        assert!(guidance.contains("add `--background` to detach it"));
        assert!(guidance.contains("--scope all --background --progress"));
        assert_eq!(
            fallback,
            "run `muralctl world cache compute --scope all --background --progress`"
        );
    }

    #[test]
    fn wallpaper_entries_for_set_outputs_are_stable() {
        let outputs = BTreeMap::from([
            ("DP-2".to_owned(), "/wall/b.jpg".to_owned()),
            ("DP-1".to_owned(), "/wall/a.jpg".to_owned()),
        ]);

        let entries = wallpaper_entries_for_set_outputs(&outputs);

        assert_eq!(
            entries,
            vec![
                WallpaperEntry {
                    index: 0,
                    output: "DP-1".to_owned(),
                    favorite: false,
                    path: "/wall/a.jpg".to_owned(),
                },
                WallpaperEntry {
                    index: 1,
                    output: "DP-2".to_owned(),
                    favorite: false,
                    path: "/wall/b.jpg".to_owned(),
                },
            ]
        );
    }
}
