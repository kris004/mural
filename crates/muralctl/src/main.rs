use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fmt::Write as _;
use std::fs::{self, OpenOptions};
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::path::PathBuf;
use std::process::{self, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use image::imageops::FilterType;
use image::{ImageFormat, Rgba, RgbaImage};
use mural_core::MuralConfig;
use mural_core::world_cache::{
    DEFAULT_WORLD_CELL_THUMBNAIL_EDGE, DEFAULT_WORLD_TILE_CELLS, WorldCacheStatus,
    WorldCellCacheEntry, WorldLodCacheStatus, WorldLodPlan, WorldTileCacheEntry,
    world_cache_status, world_cell_cache_entries, world_lod_tile_cells,
    world_tile_pyramid_cache_entries, write_world_cache_index,
};
use mural_ipc::{
    CacheAction, CacheBackend, CacheRequest, CacheWarmScope, CanvasMode, CanvasPanAxis,
    CanvasTileCount, CanvasWalk, ClearRequest, DEFAULT_CANVAS_CACHE_WORKERS, DEFAULT_CANVAS_IN_MS,
    DEFAULT_CANVAS_OUT_MS, DEFAULT_CANVAS_OVERVIEW_SCALE, DEFAULT_CANVAS_PAN_MS,
    DEFAULT_DURATION_MS, Easing, MAX_CANVAS_TILE_COUNT, PreloadRequest, PushMode, Request,
    ScaleMode, SetRequest, Transition, WallpaperAction, WallpaperRequest, default_socket_path,
    response_is_error, validate_canvas_mode_walk,
};
use mural_render::{WorldLayout, world_tiles_for_route};

mod help;
mod output;
mod transport;

static WORLD_VIPS_TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

use help::{
    print_cache_help, print_capabilities_help, print_help, print_set_help, print_wallpaper_help,
};
use output::{PrintMode, print_response};
use transport::{DEFAULT_TIMEOUT, send_request};

const MAX_WORLD_ROUTE_TILES: usize = 16;
const WORLD_CACHE_FAILURE_LOG: &str = "last-compute-failures.tsv";

fn main() {
    match run() {
        Ok(exit_code) => process::exit(exit_code),
        Err(error) => {
            eprintln!("muralctl: {error}");
            process::exit(1);
        }
    }
}

fn run() -> Result<i32, String> {
    let (socket_path, timeout, args) = extract_global_options(env::args().skip(1).collect())?;
    if args.is_empty() {
        print_help();
        return Ok(0);
    }
    if matches!(args.as_slice(), [arg] if matches!(arg.as_str(), "-V" | "--version")) {
        println!("muralctl {}", env!("CARGO_PKG_VERSION"));
        return Ok(0);
    }
    if args[0] == "world" {
        return run_world_command(&args[1..]);
    }

    let Some(command) = build_command(&args)? else {
        return Ok(0);
    };

    let socket_path = socket_path.map_or_else(default_socket_path, Ok)?;
    let response = send_request(&socket_path, &command.request, timeout)?;
    print_response(&response, command.print_mode)?;

    Ok(if response_is_error(&response) { 2 } else { 0 })
}

fn extract_global_options(
    args: Vec<String>,
) -> Result<(Option<PathBuf>, Duration, Vec<String>), String> {
    let mut socket_path = None;
    let mut timeout = DEFAULT_TIMEOUT;
    let mut filtered = Vec::with_capacity(args.len());
    let mut args = args.into_iter();

    while let Some(arg) = args.next() {
        if arg == "--socket" {
            let value = args
                .next()
                .ok_or_else(|| "--socket requires a path".to_owned())?;
            socket_path = Some(PathBuf::from(value));
        } else if arg == "--timeout-ms" {
            let value = args
                .next()
                .ok_or_else(|| "--timeout-ms requires a value".to_owned())?;
            timeout = parse_timeout_ms(&value)?;
        } else {
            filtered.push(arg);
        }
    }

    Ok((socket_path, timeout, filtered))
}

fn parse_timeout_ms(value: &str) -> Result<Duration, String> {
    let millis = value
        .parse::<u64>()
        .map_err(|_| format!("--timeout-ms must be a positive integer: {value}"))?;
    if millis == 0 {
        return Err("--timeout-ms must be greater than zero".to_owned());
    }
    Ok(Duration::from_millis(millis))
}

#[derive(Clone, Debug, PartialEq)]
struct BuiltCommand {
    request: Request,
    print_mode: PrintMode,
}

fn build_command(args: &[String]) -> Result<Option<BuiltCommand>, String> {
    let command = args[0].as_str();
    let rest = &args[1..];

    if command == "capabilities" {
        return parse_capabilities_command(rest);
    }

    let request = match command {
        "ping" => parse_ping(rest),
        "health" => parse_health(rest),
        "query" => parse_query(rest),
        "set" => parse_set(rest),
        "preload" => parse_preload(rest),
        "clear" => parse_clear(rest),
        "cache" => parse_cache(rest),
        "stop" => parse_stop(rest),
        "-h" | "--help" => {
            print_help();
            Ok(None)
        }
        _ => Err(format!("unknown command: {command}")),
    };

    match request {
        Ok(Some(request)) => Ok(Some(BuiltCommand {
            request,
            print_mode: PrintMode::RawJson,
        })),
        Ok(None) => Ok(None),
        Err(_) if is_wallpaper_command(command) => parse_wallpaper_command(command, rest),
        Err(error) => Err(error),
    }
}

fn parse_capabilities_command(args: &[String]) -> Result<Option<BuiltCommand>, String> {
    let mut json = false;
    for arg in args {
        match arg.as_str() {
            "--json" => json = true,
            "-h" | "--help" => {
                print_capabilities_help();
                return Ok(None);
            }
            _ => return Err(format!("capabilities does not accept argument: {arg}")),
        }
    }

    Ok(Some(BuiltCommand {
        request: Request::Capabilities,
        print_mode: if json {
            PrintMode::RawJson
        } else {
            PrintMode::CapabilitiesText
        },
    }))
}

fn is_wallpaper_command(command: &str) -> bool {
    matches!(
        command,
        "next"
            | "back"
            | "shift"
            | "shift-forward"
            | "shift-back"
            | "replace"
            | "quarantine"
            | "quarentine"
            | "favorite"
            | "unfavorite"
            | "favorites"
            | "current"
            | "rescan"
    )
}

fn parse_health(args: &[String]) -> Result<Option<Request>, String> {
    for arg in args {
        if matches!(arg.as_str(), "-h" | "--help") {
            println!(
                "USAGE:\n    muralctl health [--json]\n\nPrint supervisor and renderer health as JSON."
            );
            return Ok(None);
        }
        if arg != "--json" {
            return Err(format!("health does not accept argument: {arg}"));
        }
    }
    Ok(Some(Request::Health))
}

fn parse_wallpaper_command(command: &str, args: &[String]) -> Result<Option<BuiltCommand>, String> {
    if args.iter().any(|arg| arg == "-h" || arg == "--help") {
        print_wallpaper_help(command);
        return Ok(None);
    }

    let (action, options) = match command {
        "next" => {
            let parsed = parse_wallpaper_options(args, "push:up")?;
            require_positional_count(command, &parsed.positionals, 0)?;
            (WallpaperAction::Next, parsed)
        }
        "back" => {
            let parsed = parse_wallpaper_options(args, "push:down")?;
            require_positional_count(command, &parsed.positionals, 0)?;
            (WallpaperAction::Back, parsed)
        }
        "shift" => {
            let parsed = parse_wallpaper_options(args, "push:left")?;
            let direction = parsed.positionals.first().map_or("forward", String::as_str);
            if parsed.positionals.len() > 1 {
                return Err("shift accepts at most one direction".to_owned());
            }
            let (action, transition) = match direction {
                "f" | "forward" => (WallpaperAction::ShiftForward, "push:left"),
                "b" | "back" | "backward" => (WallpaperAction::ShiftBack, "push:right"),
                _ => {
                    return Err(
                        "shift direction must be one of: forward, f, back, backward, b".to_owned(),
                    );
                }
            };
            (action, parsed.with_default_transition(transition)?)
        }
        "shift-forward" => {
            let parsed = parse_wallpaper_options(args, "push:left")?;
            require_positional_count(command, &parsed.positionals, 0)?;
            (WallpaperAction::ShiftForward, parsed)
        }
        "shift-back" => {
            let parsed = parse_wallpaper_options(args, "push:right")?;
            require_positional_count(command, &parsed.positionals, 0)?;
            (WallpaperAction::ShiftBack, parsed)
        }
        "replace" => {
            let parsed = parse_wallpaper_options(args, "cut")?;
            let index = parse_single_index(command, &parsed.positionals)?;
            (WallpaperAction::Replace { index }, parsed)
        }
        "quarantine" | "quarentine" => {
            let parsed = parse_wallpaper_options(args, "cut")?;
            let index = parse_single_index(command, &parsed.positionals)?;
            (WallpaperAction::Quarantine { index }, parsed)
        }
        "favorite" => {
            let parsed = parse_json_only_options(args, "favorite")?;
            let index = parse_single_index(command, &parsed.positionals)?;
            (WallpaperAction::Favorite { index }, parsed)
        }
        "unfavorite" => {
            let parsed = parse_json_only_options(args, "unfavorite")?;
            let index = parse_single_index(command, &parsed.positionals)?;
            (WallpaperAction::Unfavorite { index }, parsed)
        }
        "favorites" => {
            let parsed = parse_json_only_options(args, "favorites")?;
            require_positional_count(command, &parsed.positionals, 0)?;
            (WallpaperAction::Favorites, parsed)
        }
        "current" => {
            let parsed = parse_json_only_options(args, "current")?;
            require_positional_count(command, &parsed.positionals, 0)?;
            (WallpaperAction::Current, parsed)
        }
        "rescan" => {
            let parsed = parse_json_only_options(args, "rescan")?;
            require_positional_count(command, &parsed.positionals, 0)?;
            (WallpaperAction::Rescan, parsed)
        }
        _ => return Err(format!("unknown command: {command}")),
    };

    Ok(Some(BuiltCommand {
        request: Request::Wallpaper(WallpaperRequest {
            action,
            transition: options.transition,
            scale_mode: options.scale_mode,
        }),
        print_mode: if options.json {
            PrintMode::WallpaperJson
        } else {
            PrintMode::WallpaperText
        },
    }))
}

#[derive(Clone, Debug, PartialEq)]
struct WallpaperCliOptions {
    positionals: Vec<String>,
    transition_token: String,
    transition_explicit: bool,
    transition_token_explicit: bool,
    duration_ms: u64,
    easing: Easing,
    mode: Option<String>,
    canvas_out_ms: u64,
    canvas_pan_ms: u64,
    canvas_in_ms: u64,
    canvas_walk: Option<CanvasWalk>,
    canvas_pan_axis: Option<CanvasPanAxis>,
    canvas_overview_scale: f32,
    canvas_tile_count: CanvasTileCount,
    scale_mode: Option<ScaleMode>,
    transition: Option<Transition>,
    json: bool,
}

impl WallpaperCliOptions {
    fn with_default_transition(mut self, default_transition: &str) -> Result<Self, String> {
        if !self.transition_explicit {
            default_transition.clone_into(&mut self.transition_token);
            self.transition = None;
        } else if !self.transition_token_explicit {
            default_transition.clone_into(&mut self.transition_token);
            self.transition = Some(build_transition(
                &self.transition_token,
                self.duration_ms,
                self.easing,
                self.mode.as_deref(),
                self.canvas_out_ms,
                self.canvas_pan_ms,
                self.canvas_in_ms,
                self.canvas_walk,
                self.canvas_pan_axis,
                self.canvas_overview_scale,
                self.canvas_tile_count,
            )?);
        }
        Ok(self)
    }
}

#[allow(clippy::too_many_lines)]
fn parse_wallpaper_options(
    args: &[String],
    default_transition: &str,
) -> Result<WallpaperCliOptions, String> {
    let mut positionals = Vec::new();
    let mut transition_token = String::new();
    let mut transition_explicit = false;
    let mut transition_token_explicit = false;
    let mut duration_ms = DEFAULT_DURATION_MS;
    let mut easing = Easing::EaseOutCubic;
    let mut mode = None;
    let mut canvas_out_ms = DEFAULT_CANVAS_OUT_MS;
    let mut canvas_pan_ms = DEFAULT_CANVAS_PAN_MS;
    let mut canvas_in_ms = DEFAULT_CANVAS_IN_MS;
    let mut canvas_walk = None;
    let mut canvas_pan_axis = None;
    let mut canvas_overview_scale = DEFAULT_CANVAS_OVERVIEW_SCALE;
    let mut canvas_tile_count = CanvasTileCount::Auto { max: None };
    let mut canvas_option_explicit = false;
    let mut scale_mode = None;
    let mut json = false;
    let mut index = 0;

    while index < args.len() {
        match args[index].as_str() {
            "--json" => json = true,
            "-t" | "--transition" => {
                index += 1;
                transition_explicit = true;
                transition_token_explicit = true;
                transition_token.clone_from(
                    args.get(index)
                        .ok_or_else(|| "--transition requires a value".to_owned())?,
                );
            }
            "--duration-ms" => {
                index += 1;
                transition_explicit = true;
                duration_ms = parse_duration(
                    args.get(index)
                        .ok_or_else(|| "--duration-ms requires milliseconds".to_owned())?,
                )?;
            }
            "--easing" => {
                index += 1;
                transition_explicit = true;
                easing = Easing::parse(
                    args.get(index)
                        .ok_or_else(|| "--easing requires a value".to_owned())?,
                )
                .map_err(|error| error.to_string())?;
            }
            "--mode" => {
                index += 1;
                transition_explicit = true;
                mode = Some(
                    args.get(index)
                        .ok_or_else(|| "--mode requires a value".to_owned())?
                        .clone(),
                );
            }
            "--canvas-zoom-out-ms" => {
                index += 1;
                transition_explicit = true;
                canvas_option_explicit = true;
                if transition_token.is_empty() {
                    "canvas".clone_into(&mut transition_token);
                    transition_token_explicit = true;
                }
                canvas_out_ms = parse_duration(
                    args.get(index)
                        .ok_or_else(|| "--canvas-zoom-out-ms requires milliseconds".to_owned())?,
                )?;
            }
            "--canvas-pan-ms" => {
                index += 1;
                transition_explicit = true;
                canvas_option_explicit = true;
                if transition_token.is_empty() {
                    "canvas".clone_into(&mut transition_token);
                    transition_token_explicit = true;
                }
                canvas_pan_ms = parse_duration(
                    args.get(index)
                        .ok_or_else(|| "--canvas-pan-ms requires milliseconds".to_owned())?,
                )?;
            }
            "--canvas-pan-axis" => {
                index += 1;
                transition_explicit = true;
                canvas_option_explicit = true;
                if transition_token.is_empty() {
                    "canvas".clone_into(&mut transition_token);
                    transition_token_explicit = true;
                }
                canvas_pan_axis = Some(parse_canvas_pan_axis(
                    args.get(index)
                        .ok_or_else(|| "--canvas-pan-axis requires a value".to_owned())?,
                )?);
            }
            "--canvas-walk" => {
                index += 1;
                transition_explicit = true;
                canvas_option_explicit = true;
                if transition_token.is_empty() {
                    "canvas".clone_into(&mut transition_token);
                    transition_token_explicit = true;
                }
                canvas_walk = Some(
                    CanvasWalk::parse(
                        args.get(index)
                            .ok_or_else(|| "--canvas-walk requires a value".to_owned())?,
                    )
                    .map_err(|error| error.to_string())?,
                );
            }
            "--canvas-zoom-in-ms" => {
                index += 1;
                transition_explicit = true;
                canvas_option_explicit = true;
                if transition_token.is_empty() {
                    "canvas".clone_into(&mut transition_token);
                    transition_token_explicit = true;
                }
                canvas_in_ms = parse_duration(
                    args.get(index)
                        .ok_or_else(|| "--canvas-zoom-in-ms requires milliseconds".to_owned())?,
                )?;
            }
            "--canvas-overview-scale" => {
                index += 1;
                transition_explicit = true;
                canvas_option_explicit = true;
                if transition_token.is_empty() {
                    "canvas".clone_into(&mut transition_token);
                    transition_token_explicit = true;
                }
                canvas_overview_scale = parse_canvas_overview_scale(
                    args.get(index)
                        .ok_or_else(|| "--canvas-overview-scale requires a value".to_owned())?,
                )?;
            }
            "--canvas-tile-count" => {
                index += 1;
                transition_explicit = true;
                canvas_option_explicit = true;
                if transition_token.is_empty() {
                    "canvas".clone_into(&mut transition_token);
                    transition_token_explicit = true;
                }
                let parsed = parse_canvas_tile_count(
                    args.get(index)
                        .ok_or_else(|| "--canvas-tile-count requires a value".to_owned())?,
                )?;
                canvas_tile_count = match (parsed, canvas_tile_count) {
                    (CanvasTileCount::Auto { .. }, CanvasTileCount::Auto { max }) => {
                        CanvasTileCount::Auto { max }
                    }
                    (parsed, _) => parsed,
                };
            }
            "--canvas-max-tile-count" => {
                index += 1;
                transition_explicit = true;
                canvas_option_explicit = true;
                if transition_token.is_empty() {
                    "canvas".clone_into(&mut transition_token);
                    transition_token_explicit = true;
                }
                let max = parse_canvas_max_tile_count(
                    args.get(index)
                        .ok_or_else(|| "--canvas-max-tile-count requires a value".to_owned())?,
                )?;
                canvas_tile_count = match canvas_tile_count {
                    CanvasTileCount::Auto { .. } => CanvasTileCount::Auto { max: Some(max) },
                    CanvasTileCount::Fixed(_) => canvas_tile_count,
                };
            }
            "--scale-mode" => {
                index += 1;
                scale_mode = Some(
                    ScaleMode::parse(
                        args.get(index)
                            .ok_or_else(|| "--scale-mode requires a value".to_owned())?,
                    )
                    .map_err(|error| error.to_string())?,
                );
            }
            arg if arg.starts_with('-') => {
                return Err(format!("unknown wallpaper option: {arg}"));
            }
            value => positionals.push(value.to_owned()),
        }
        index += 1;
    }

    let transition = if transition_explicit {
        if transition_token.is_empty() {
            default_transition.clone_into(&mut transition_token);
        }
        if canvas_option_explicit && !is_canvas_transition_token(&transition_token) {
            return Err("canvas options require --transition canvas".to_owned());
        }
        Some(build_transition(
            &transition_token,
            duration_ms,
            easing,
            mode.as_deref(),
            canvas_out_ms,
            canvas_pan_ms,
            canvas_in_ms,
            canvas_walk,
            canvas_pan_axis,
            canvas_overview_scale,
            canvas_tile_count,
        )?)
    } else {
        None
    };
    if transition_token.is_empty() {
        default_transition.clone_into(&mut transition_token);
    }

    Ok(WallpaperCliOptions {
        positionals,
        transition_token,
        transition_explicit,
        transition_token_explicit,
        duration_ms,
        easing,
        mode,
        canvas_out_ms,
        canvas_pan_ms,
        canvas_in_ms,
        canvas_walk,
        canvas_pan_axis,
        canvas_overview_scale,
        canvas_tile_count,
        scale_mode,
        transition,
        json,
    })
}

fn parse_json_only_options(args: &[String], command: &str) -> Result<WallpaperCliOptions, String> {
    let mut positionals = Vec::new();
    let mut json = false;

    for arg in args {
        match arg.as_str() {
            "--json" => json = true,
            value if value.starts_with('-') => {
                return Err(format!("{command} does not accept option: {value}"));
            }
            value => positionals.push(value.to_owned()),
        }
    }

    Ok(WallpaperCliOptions {
        positionals,
        transition_token: "cut".to_owned(),
        transition_explicit: false,
        transition_token_explicit: false,
        duration_ms: DEFAULT_DURATION_MS,
        easing: Easing::EaseOutCubic,
        mode: None,
        canvas_out_ms: DEFAULT_CANVAS_OUT_MS,
        canvas_pan_ms: DEFAULT_CANVAS_PAN_MS,
        canvas_in_ms: DEFAULT_CANVAS_IN_MS,
        canvas_walk: None,
        canvas_pan_axis: None,
        canvas_overview_scale: DEFAULT_CANVAS_OVERVIEW_SCALE,
        canvas_tile_count: CanvasTileCount::Auto { max: None },
        scale_mode: None,
        transition: None,
        json,
    })
}

#[allow(clippy::too_many_arguments)]
fn build_transition(
    transition_token: &str,
    duration_ms: u64,
    easing: Easing,
    mode: Option<&str>,
    canvas_out_ms: u64,
    canvas_pan_ms: u64,
    canvas_in_ms: u64,
    canvas_walk: Option<CanvasWalk>,
    canvas_pan_axis: Option<CanvasPanAxis>,
    canvas_overview_scale: f32,
    canvas_tile_count: CanvasTileCount,
) -> Result<Transition, String> {
    let mut transition = Transition::parse_cli_token(transition_token, duration_ms, easing)
        .map_err(|error| error.to_string())?;
    match &mut transition {
        Transition::Push {
            mode: push_mode, ..
        } => {
            if let Some(mode) = mode {
                *push_mode = PushMode::parse(mode).map_err(|error| error.to_string())?;
            }
        }
        Transition::Canvas {
            zoom_out_ms,
            pan_ms,
            zoom_in_ms,
            mode: canvas_mode,
            walk,
            pan_axis,
            overview_scale,
            tile_count,
            ..
        } => {
            *zoom_out_ms = canvas_out_ms;
            *pan_ms = canvas_pan_ms;
            *zoom_in_ms = canvas_in_ms;
            if let Some(canvas_pan_axis) = canvas_pan_axis {
                *pan_axis = canvas_pan_axis;
            }
            if let Some(mode) = mode {
                *canvas_mode = CanvasMode::parse(mode).map_err(|error| error.to_string())?;
            }
            if let Some(canvas_walk) = canvas_walk {
                *walk = canvas_walk;
            }
            validate_canvas_mode_walk(*canvas_mode, *walk).map_err(|error| error.to_string())?;
            *overview_scale = canvas_overview_scale;
            *tile_count = canvas_tile_count;
        }
        Transition::Cut | Transition::Fade { .. } | Transition::World { .. } => {
            if mode.is_some() {
                return Err("--mode requires a push or canvas transition".to_owned());
            }
        }
    }
    Ok(transition)
}

fn require_positional_count(
    command: &str,
    positionals: &[String],
    count: usize,
) -> Result<(), String> {
    if positionals.len() == count {
        Ok(())
    } else {
        Err(format!(
            "{command} expects {count} positional argument(s), got {}",
            positionals.len()
        ))
    }
}

fn parse_single_index(command: &str, positionals: &[String]) -> Result<usize, String> {
    require_positional_count(command, positionals, 1)?;
    positionals[0]
        .parse::<usize>()
        .map_err(|error| format!("{command} requires a 0-based integer index: {error}"))
}

fn parse_ping(args: &[String]) -> Result<Option<Request>, String> {
    if args.iter().any(|arg| arg == "-h" || arg == "--help") {
        println!("USAGE:\n    muralctl ping [--socket PATH] [--timeout-ms MS]");
        return Ok(None);
    }
    require_no_args(args, "ping")?;
    Ok(Some(Request::Ping))
}

fn parse_query(args: &[String]) -> Result<Option<Request>, String> {
    for arg in args {
        match arg.as_str() {
            "--json" => {}
            "-h" | "--help" => {
                println!("USAGE:\n    muralctl query [--json] [--socket PATH] [--timeout-ms MS]");
                return Ok(None);
            }
            _ => return Err(format!("query does not accept argument: {arg}")),
        }
    }

    Ok(Some(Request::Query))
}

#[allow(clippy::too_many_lines)]
fn parse_set(args: &[String]) -> Result<Option<Request>, String> {
    if args.iter().any(|arg| arg == "-h" || arg == "--help") {
        print_set_help();
        return Ok(None);
    }

    let mut outputs = BTreeMap::new();
    let mut pending_output = None;
    let mut transition_token = "cut".to_owned();
    let mut duration_ms = DEFAULT_DURATION_MS;
    let mut easing = Easing::EaseOutCubic;
    let mut mode = None;
    let mut canvas_option_explicit = false;
    let mut scale_mode = ScaleMode::Fill;
    let mut allow_partial = false;
    let mut index = 0;

    while index < args.len() {
        match args[index].as_str() {
            "-o" | "--output" => {
                parse_output_path_option(args, &mut index, &mut pending_output, &mut outputs)?;
            }
            "-t" | "--transition" => {
                index += 1;
                transition_token.clone_from(
                    args.get(index)
                        .ok_or_else(|| "--transition requires a value".to_owned())?,
                );
            }
            "--duration-ms" => {
                index += 1;
                duration_ms = parse_duration(
                    args.get(index)
                        .ok_or_else(|| "--duration-ms requires milliseconds".to_owned())?,
                )?;
            }
            "--easing" => {
                index += 1;
                easing = Easing::parse(
                    args.get(index)
                        .ok_or_else(|| "--easing requires a value".to_owned())?,
                )
                .map_err(|error| error.to_string())?;
            }
            "--mode" => {
                index += 1;
                mode = Some(
                    args.get(index)
                        .ok_or_else(|| "--mode requires a value".to_owned())?
                        .clone(),
                );
            }
            "--canvas-zoom-out-ms" => {
                index += 1;
                canvas_option_explicit = true;
                if transition_token == "cut" {
                    "canvas".clone_into(&mut transition_token);
                }
                let _ = parse_duration(
                    args.get(index)
                        .ok_or_else(|| "--canvas-zoom-out-ms requires milliseconds".to_owned())?,
                )?;
            }
            "--canvas-pan-ms" => {
                index += 1;
                canvas_option_explicit = true;
                if transition_token == "cut" {
                    "canvas".clone_into(&mut transition_token);
                }
                let _ = parse_duration(
                    args.get(index)
                        .ok_or_else(|| "--canvas-pan-ms requires milliseconds".to_owned())?,
                )?;
            }
            "--canvas-pan-axis" => {
                index += 1;
                canvas_option_explicit = true;
                if transition_token == "cut" {
                    "canvas".clone_into(&mut transition_token);
                }
                let _ = parse_canvas_pan_axis(
                    args.get(index)
                        .ok_or_else(|| "--canvas-pan-axis requires a value".to_owned())?,
                )?;
            }
            "--canvas-zoom-in-ms" => {
                index += 1;
                canvas_option_explicit = true;
                if transition_token == "cut" {
                    "canvas".clone_into(&mut transition_token);
                }
                let _ = parse_duration(
                    args.get(index)
                        .ok_or_else(|| "--canvas-zoom-in-ms requires milliseconds".to_owned())?,
                )?;
            }
            "--canvas-overview-scale" => {
                index += 1;
                canvas_option_explicit = true;
                if transition_token == "cut" {
                    "canvas".clone_into(&mut transition_token);
                }
                let _ = parse_canvas_overview_scale(
                    args.get(index)
                        .ok_or_else(|| "--canvas-overview-scale requires a value".to_owned())?,
                )?;
            }
            "--canvas-tile-count" => {
                index += 1;
                canvas_option_explicit = true;
                if transition_token == "cut" {
                    "canvas".clone_into(&mut transition_token);
                }
                let _ = parse_canvas_tile_count(
                    args.get(index)
                        .ok_or_else(|| "--canvas-tile-count requires a value".to_owned())?,
                )?;
            }
            "--canvas-max-tile-count" => {
                index += 1;
                canvas_option_explicit = true;
                if transition_token == "cut" {
                    "canvas".clone_into(&mut transition_token);
                }
                let _ = parse_canvas_max_tile_count(
                    args.get(index)
                        .ok_or_else(|| "--canvas-max-tile-count requires a value".to_owned())?,
                )?;
            }
            "--scale-mode" => {
                index += 1;
                scale_mode = ScaleMode::parse(
                    args.get(index)
                        .ok_or_else(|| "--scale-mode requires a value".to_owned())?,
                )
                .map_err(|error| error.to_string())?;
            }
            "--allow-partial" => allow_partial = true,
            arg if arg.starts_with('-') => return Err(format!("unknown set option: {arg}")),
            path => assign_positional_path(path, &mut pending_output, &mut outputs)?,
        }
        index += 1;
    }

    if let Some(output) = pending_output {
        return Err(format!("missing image path for output {output}"));
    }
    if outputs.is_empty() {
        return Err(
            "set requires at least one --output NAME PATH or --output NAME=PATH".to_owned(),
        );
    }

    let mut transition = Transition::parse_cli_token(&transition_token, duration_ms, easing)
        .map_err(|error| error.to_string())?;
    if canvas_option_explicit && !is_canvas_transition_token(&transition_token) {
        return Err("canvas options require --transition canvas".to_owned());
    }
    if matches!(transition, Transition::Canvas { .. }) {
        return Err(
            "canvas transitions are only supported for wallpaper navigation actions; use next/back/shift/replace/quarantine, or use set with cut, push, or world"
                .to_owned(),
        );
    }
    match &mut transition {
        Transition::Push {
            mode: push_mode, ..
        } => {
            if let Some(mode) = &mode {
                *push_mode = PushMode::parse(mode).map_err(|error| error.to_string())?;
            }
        }
        Transition::Cut | Transition::Fade { .. } | Transition::World { .. } => {
            if mode.is_some() {
                return Err("--mode requires a push transition for set".to_owned());
            }
        }
        Transition::Canvas { .. } => unreachable!("canvas set transitions are rejected above"),
    }

    Ok(Some(Request::Set(SetRequest {
        outputs,
        transition,
        scale_mode,
        allow_partial,
    })))
}

fn parse_preload(args: &[String]) -> Result<Option<Request>, String> {
    if args.iter().any(|arg| arg == "-h" || arg == "--help") {
        println!(
            "USAGE:\n    muralctl preload --output NAME PATH [--output NAME=PATH ...] [--socket PATH] [--timeout-ms MS]"
        );
        return Ok(None);
    }

    let outputs = parse_output_path_args(args, "preload")?;
    Ok(Some(Request::Preload(PreloadRequest { outputs })))
}

fn parse_clear(args: &[String]) -> Result<Option<Request>, String> {
    if args.iter().any(|arg| arg == "-h" || arg == "--help") {
        println!(
            "USAGE:\n    muralctl clear [--output NAME ...] [--color #000000] [--socket PATH] [--timeout-ms MS]"
        );
        return Ok(None);
    }

    let mut outputs = Vec::new();
    let mut color = "#000000".to_owned();
    let mut index = 0;

    while index < args.len() {
        match args[index].as_str() {
            "-o" | "--output" => {
                index += 1;
                let output = args
                    .get(index)
                    .ok_or_else(|| "--output requires an output name".to_owned())?;
                if output.contains('=') {
                    return Err("clear --output accepts only an output name".to_owned());
                }
                if output.is_empty() {
                    return Err("output name must not be empty".to_owned());
                }
                outputs.push(output.clone());
            }
            "--color" => {
                index += 1;
                color.clone_from(
                    args.get(index)
                        .ok_or_else(|| "--color requires a value".to_owned())?,
                );
            }
            arg => return Err(format!("clear does not accept argument: {arg}")),
        }
        index += 1;
    }

    Ok(Some(Request::Clear(ClearRequest { outputs, color })))
}

fn parse_cache(args: &[String]) -> Result<Option<Request>, String> {
    if args.is_empty() || args.iter().any(|arg| arg == "-h" || arg == "--help") {
        print_cache_help();
        return Ok(None);
    }

    let action = args[0].as_str();
    match action {
        "status" => {
            for arg in &args[1..] {
                match arg.as_str() {
                    "--json" => {}
                    _ => return Err(format!("cache status does not accept argument: {arg}")),
                }
            }
            Ok(Some(Request::Cache(CacheRequest {
                action: CacheAction::Status,
            })))
        }
        "clear" => {
            for arg in &args[1..] {
                match arg.as_str() {
                    "--json" => {}
                    _ => return Err(format!("cache clear does not accept argument: {arg}")),
                }
            }
            Ok(Some(Request::Cache(CacheRequest {
                action: CacheAction::Clear,
            })))
        }
        "warm" => {
            let mut scope = CacheWarmScope::Current;
            let mut workers = DEFAULT_CANVAS_CACHE_WORKERS;
            let mut backend = CacheBackend::Auto;
            let mut index = 1;

            while index < args.len() {
                match args[index].as_str() {
                    "--json" => {}
                    "--scope" => {
                        index += 1;
                        scope = CacheWarmScope::parse(
                            args.get(index)
                                .ok_or_else(|| "--scope requires current or all".to_owned())?,
                        )
                        .map_err(|error| error.to_string())?;
                    }
                    "--workers" => {
                        index += 1;
                        workers =
                            parse_cache_workers(args.get(index).ok_or_else(|| {
                                "--workers requires a positive integer".to_owned()
                            })?)?;
                    }
                    "--backend" => {
                        index += 1;
                        backend = CacheBackend::parse(args.get(index).ok_or_else(|| {
                            "--backend requires auto, vips, or internal".to_owned()
                        })?)
                        .map_err(|error| error.to_string())?;
                    }
                    arg if arg.starts_with('-') => {
                        return Err(format!("unknown cache warm option: {arg}"));
                    }
                    arg => return Err(format!("cache warm does not accept argument: {arg}")),
                }
                index += 1;
            }

            Ok(Some(Request::Cache(CacheRequest {
                action: CacheAction::Warm {
                    scope,
                    workers,
                    backend,
                },
            })))
        }
        _ => Err(format!("unknown cache action: {action}")),
    }
}

fn parse_stop(args: &[String]) -> Result<Option<Request>, String> {
    if args.iter().any(|arg| arg == "-h" || arg == "--help") {
        println!("USAGE:\n    muralctl stop [--socket PATH] [--timeout-ms MS]");
        return Ok(None);
    }
    require_no_args(args, "stop")?;
    Ok(Some(Request::Stop))
}

fn run_world_command(args: &[String]) -> Result<i32, String> {
    if args.is_empty() || args.iter().any(|arg| arg == "-h" || arg == "--help") {
        print_world_help();
        return Ok(0);
    }
    if args[0] != "cache" {
        return Err(format!("unknown world action: {}", args[0]));
    }
    run_world_cache_command(&args[1..])
}

fn run_world_cache_command(args: &[String]) -> Result<i32, String> {
    if args.is_empty() || args.iter().any(|arg| arg == "-h" || arg == "--help") {
        print_world_cache_help();
        return Ok(0);
    }

    match args[0].as_str() {
        "status" => {
            let json = parse_flag_only(&args[1..], "world cache status", "--json")?;
            let config = MuralConfig::load()?;
            let status = world_cache_status(&config)?;
            print_world_cache_status(&status, json);
            Ok(if status.ready { 0 } else { 2 })
        }
        "index" => {
            let json = parse_flag_only(&args[1..], "world cache index", "--json")?;
            let config = MuralConfig::load()?;
            let status = write_world_cache_index(&config)?;
            print_world_cache_status(&status, json);
            Ok(0)
        }
        "failures" => {
            let json = parse_flag_only(&args[1..], "world cache failures", "--json")?;
            let config = MuralConfig::load()?;
            let status = world_cache_status(&config)?;
            let failures = read_world_cache_failure_records(&status)?;
            print_world_cache_failures(&status, &failures, json);
            Ok(if failures.is_empty() { 0 } else { 2 })
        }
        "compute" => parse_world_cache_compute(&args[1..]),
        action => Err(format!("unknown world cache action: {action}")),
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum WorldCacheComputeScope {
    All,
    Route {
        routes: Vec<WorldCacheRoute>,
    },
    Neighborhood {
        centers: Vec<String>,
        radius: usize,
        lod: usize,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct WorldCacheRoute {
    from: String,
    to: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WorldCacheComputeExecution {
    Foreground,
    Background,
}

#[derive(Debug)]
struct WorldCacheComputeOptions {
    dry_run: bool,
    execution: WorldCacheComputeExecution,
    progress: bool,
    json: bool,
    workers: usize,
    limit: Option<usize>,
    tile_limit: Option<usize>,
    scope: WorldCacheComputeScope,
}

fn parse_world_cache_compute(args: &[String]) -> Result<i32, String> {
    let options = parse_world_cache_compute_options(args)?;
    let config = MuralConfig::load()?;
    if options.execution == WorldCacheComputeExecution::Background {
        let launched = launch_background_world_cache_compute(&config, args, options.json)?;
        print_background_world_cache_compute(&launched, options.json);
        return Ok(0);
    }
    let plan = plan_world_cache_compute_for_scope(
        &config,
        &options.scope,
        options.limit,
        options.tile_limit,
        options.dry_run,
    )?;
    if options.dry_run {
        print_world_cache_plan(&plan, options.progress, options.json);
        return Ok(0);
    }

    let result = compute_world_cache(
        &config,
        &options.scope,
        options.limit,
        options.tile_limit,
        options.workers,
        options.progress,
    )?;
    if options.json {
        println!(
            "{{\"cell_generated\":{},\"cell_skipped\":{},\"tile_generated\":{},\"tile_skipped\":{},\"failed\":{},\"limited\":{},\"thumbnail_edge\":{},\"tile_cells\":{}}}",
            result.cell_generated,
            result.cell_skipped,
            result.tile_generated,
            result.tile_skipped,
            result.failed,
            result.limited,
            DEFAULT_WORLD_CELL_THUMBNAIL_EDGE,
            DEFAULT_WORLD_TILE_CELLS
        );
    } else {
        println!("cell_generated\t{}", result.cell_generated);
        println!("cell_skipped\t{}", result.cell_skipped);
        println!("tile_generated\t{}", result.tile_generated);
        println!("tile_skipped\t{}", result.tile_skipped);
        println!("failed\t{}", result.failed);
        println!("limited\t{}", result.limited);
        println!("thumbnail_edge\t{DEFAULT_WORLD_CELL_THUMBNAIL_EDGE}");
        println!("tile_cells\t{DEFAULT_WORLD_TILE_CELLS}");
    }
    Ok(if result.failed == 0 { 0 } else { 2 })
}

#[allow(clippy::too_many_lines)]
fn parse_world_cache_compute_options(args: &[String]) -> Result<WorldCacheComputeOptions, String> {
    let mut dry_run = false;
    let mut execution = WorldCacheComputeExecution::Foreground;
    let mut progress = false;
    let mut json = false;
    let mut workers = DEFAULT_CANVAS_CACHE_WORKERS;
    let mut limit = None;
    let mut tile_limit = None;
    let mut scope = None;
    let mut from = None;
    let mut to = None;
    let mut routes = Vec::new();
    let mut centers = Vec::new();
    let mut neighborhood_radius = None;
    let mut neighborhood_lod = None;
    let mut index = 0;

    while index < args.len() {
        match args[index].as_str() {
            "--scope" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| "--scope requires all, route, or neighborhood".to_owned())?;
                scope = Some(match value.as_str() {
                    "all" => WorldCacheComputeScope::All,
                    "route" => WorldCacheComputeScope::Route { routes: Vec::new() },
                    "neighborhood" => WorldCacheComputeScope::Neighborhood {
                        centers: Vec::new(),
                        radius: 0,
                        lod: 0,
                    },
                    _ => {
                        return Err(
                            "world cache compute --scope must be all, route, or neighborhood"
                                .to_owned(),
                        );
                    }
                });
            }
            "--from" => {
                index += 1;
                from =
                    Some(canonical_image_path(args.get(index).ok_or_else(|| {
                        "--from requires a wallpaper path".to_owned()
                    })?)?);
            }
            "--to" => {
                index += 1;
                to =
                    Some(canonical_image_path(args.get(index).ok_or_else(|| {
                        "--to requires a wallpaper path".to_owned()
                    })?)?);
            }
            "--route" => {
                index += 1;
                let from_path =
                    canonical_image_path(args.get(index).ok_or_else(|| {
                        "--route requires FROM and TO wallpaper paths".to_owned()
                    })?)?;
                index += 1;
                let to_path =
                    canonical_image_path(args.get(index).ok_or_else(|| {
                        "--route requires FROM and TO wallpaper paths".to_owned()
                    })?)?;
                routes.push(WorldCacheRoute {
                    from: from_path,
                    to: to_path,
                });
            }
            "--center" => {
                index += 1;
                centers
                    .push(canonical_image_path(args.get(index).ok_or_else(|| {
                        "--center requires a wallpaper path".to_owned()
                    })?)?);
            }
            "--radius" => {
                index += 1;
                neighborhood_radius = Some(parse_nonnegative_usize(
                    args.get(index)
                        .ok_or_else(|| "--radius requires a non-negative integer".to_owned())?,
                )?);
            }
            "--lod" => {
                index += 1;
                neighborhood_lod = Some(parse_nonnegative_usize(
                    args.get(index)
                        .ok_or_else(|| "--lod requires a non-negative integer".to_owned())?,
                )?);
            }
            "--dry-run" => dry_run = true,
            "--background" => execution = WorldCacheComputeExecution::Background,
            "--progress" => progress = true,
            "--json" => json = true,
            "--workers" => {
                index += 1;
                workers = parse_cache_workers(
                    args.get(index)
                        .ok_or_else(|| "--workers requires a positive integer".to_owned())?,
                )?;
            }
            "--limit" => {
                index += 1;
                limit =
                    Some(parse_positive_limit(args.get(index).ok_or_else(|| {
                        "--limit requires a positive integer".to_owned()
                    })?)?);
            }
            "--tile-limit" => {
                index += 1;
                tile_limit =
                    Some(parse_positive_limit(args.get(index).ok_or_else(|| {
                        "--tile-limit requires a positive integer".to_owned()
                    })?)?);
            }
            arg if arg.starts_with('-') => {
                return Err(format!("unknown world cache compute option: {arg}"));
            }
            arg => {
                return Err(format!(
                    "world cache compute does not accept argument: {arg}"
                ));
            }
        }
        index += 1;
    }

    let scope = resolve_world_cache_compute_scope(
        scope,
        from,
        to,
        routes,
        centers,
        neighborhood_radius,
        neighborhood_lod,
    )?;
    if !matches!(scope, WorldCacheComputeScope::All) && limit.is_some() {
        return Err(
            "world cache compute --limit is only supported with --scope all; use --tile-limit to cap selected tile work"
                .to_owned(),
        );
    }
    if execution == WorldCacheComputeExecution::Background && dry_run {
        return Err(
            "world cache compute --background cannot be combined with --dry-run".to_owned(),
        );
    }
    Ok(WorldCacheComputeOptions {
        dry_run,
        execution,
        progress,
        json,
        workers,
        limit,
        tile_limit,
        scope,
    })
}

fn resolve_world_cache_compute_scope(
    scope: Option<WorldCacheComputeScope>,
    from: Option<String>,
    to: Option<String>,
    mut routes: Vec<WorldCacheRoute>,
    centers: Vec<String>,
    neighborhood_radius: Option<usize>,
    neighborhood_lod: Option<usize>,
) -> Result<WorldCacheComputeScope, String> {
    match scope.unwrap_or(WorldCacheComputeScope::All) {
        WorldCacheComputeScope::All => {
            if from.is_some()
                || to.is_some()
                || !routes.is_empty()
                || !centers.is_empty()
                || neighborhood_radius.is_some()
                || neighborhood_lod.is_some()
            {
                return Err(
                    "world cache compute route or neighborhood options require --scope route or --scope neighborhood".to_owned(),
                );
            }
            Ok(WorldCacheComputeScope::All)
        }
        WorldCacheComputeScope::Route { .. } => {
            if !centers.is_empty() || neighborhood_radius.is_some() || neighborhood_lod.is_some() {
                return Err(
                    "world cache compute --center/--radius/--lod require --scope neighborhood"
                        .to_owned(),
                );
            }
            match (from, to) {
                (Some(from), Some(to)) => routes.push(WorldCacheRoute { from, to }),
                (Some(_), None) => {
                    return Err("world cache compute --scope route requires --to".to_owned());
                }
                (None, Some(_)) => {
                    return Err("world cache compute --scope route requires --from".to_owned());
                }
                (None, None) => {}
            }
            if routes.is_empty() {
                return Err(
                    "world cache compute --scope route requires --from/--to or --route".to_owned(),
                );
            }
            Ok(WorldCacheComputeScope::Route { routes })
        }
        WorldCacheComputeScope::Neighborhood { .. } => {
            if from.is_some() || to.is_some() || !routes.is_empty() {
                return Err(
                    "world cache compute --from/--to/--route require --scope route".to_owned(),
                );
            }
            if centers.is_empty() {
                return Err("world cache compute --scope neighborhood requires --center".to_owned());
            }
            Ok(WorldCacheComputeScope::Neighborhood {
                centers,
                radius: neighborhood_radius.unwrap_or(0),
                lod: neighborhood_lod.unwrap_or(0),
            })
        }
    }
}

#[derive(Debug)]
struct BackgroundWorldCacheCompute {
    pid: u32,
    log_path: PathBuf,
}

fn launch_background_world_cache_compute(
    config: &MuralConfig,
    args: &[String],
    parent_json: bool,
) -> Result<BackgroundWorldCacheCompute, String> {
    let status = world_cache_status(config)?;
    fs::create_dir_all(&status.cache_dir).map_err(|error| {
        format!(
            "failed to create world cache directory {}: {error}",
            status.cache_dir.display()
        )
    })?;
    let log_path = background_world_cache_log_path(&status);
    let log = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&log_path)
        .map_err(|error| format!("failed to create {}: {error}", log_path.display()))?;
    let stderr = log
        .try_clone()
        .map_err(|error| format!("failed to clone {}: {error}", log_path.display()))?;

    let mut child_args = vec!["world".to_owned(), "cache".to_owned(), "compute".to_owned()];
    child_args.extend(
        args.iter()
            .filter(|arg| arg.as_str() != "--background")
            .filter(|arg| !parent_json || arg.as_str() != "--json")
            .cloned(),
    );
    let mut command = Command::new(
        env::current_exe()
            .map_err(|error| format!("failed to locate current muralctl executable: {error}"))?,
    );
    command
        .args(child_args)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(stderr));
    #[cfg(unix)]
    {
        command.process_group(0);
    }

    let child = command
        .spawn()
        .map_err(|error| format!("failed to start background world cache compute: {error}"))?;

    Ok(BackgroundWorldCacheCompute {
        pid: child.id(),
        log_path,
    })
}

fn background_world_cache_log_path(status: &WorldCacheStatus) -> PathBuf {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis());
    status
        .cache_dir
        .join(format!("background-compute-{}-{millis}.log", process::id()))
}

fn print_background_world_cache_compute(launched: &BackgroundWorldCacheCompute, json: bool) {
    if json {
        println!(
            "{{\"background_pid\":{},\"background_log\":{},\"message\":{}}}",
            launched.pid,
            json_string(&launched.log_path.display().to_string()),
            json_string("world cache compute started in background")
        );
        return;
    }
    println!("background_pid\t{}", launched.pid);
    println!("background_log\t{}", launched.log_path.display());
    println!("message\tworld cache compute started in background");
}

fn parse_flag_only(args: &[String], command: &str, flag: &str) -> Result<bool, String> {
    let mut found = false;
    for arg in args {
        if arg == flag {
            found = true;
        } else {
            return Err(format!("{command} does not accept argument: {arg}"));
        }
    }
    Ok(found)
}

fn parse_positive_limit(input: &str) -> Result<usize, String> {
    let limit = input
        .parse::<usize>()
        .map_err(|error| format!("invalid limit {input}: {error}"))?;
    if limit == 0 {
        return Err("limit must be greater than zero".to_owned());
    }
    Ok(limit)
}

fn parse_nonnegative_usize(input: &str) -> Result<usize, String> {
    input
        .parse::<usize>()
        .map_err(|error| format!("invalid integer {input}: {error}"))
}

fn print_world_help() {
    println!(
        "USAGE:\n    muralctl world cache COMMAND [OPTIONS]\n\nRun `muralctl world cache --help` for cache commands."
    );
}

fn print_world_cache_help() {
    println!(
        "USAGE:\n    muralctl world cache status [--json]\n    muralctl world cache index [--json]\n    muralctl world cache failures [--json]\n    muralctl world cache compute --scope all [--dry-run] [--background] [--limit N] [--tile-limit N] [--workers N] [--progress] [--json]\n    muralctl world cache compute --scope route --from PATH --to PATH [--dry-run] [--background] [--tile-limit N] [--workers N] [--progress] [--json]\n    muralctl world cache compute --scope route --route FROM TO [--route FROM TO ...] [--dry-run] [--background] [--tile-limit N] [--workers N] [--progress] [--json]\n    muralctl world cache compute --scope neighborhood --center PATH [--center PATH ...] [--radius N] [--lod N] [--dry-run] [--background] [--tile-limit N] [--workers N] [--progress] [--json]\n\nWorld cache commands run offline and do not require a running murald daemon."
    );
}

fn print_world_cache_status(status: &WorldCacheStatus, json: bool) {
    if json {
        println!("{}", encode_world_cache_status(status));
        return;
    }

    let failure_log = world_cache_failure_log_path(status);
    println!("wall_dir\t{}", status.wall_dir.display());
    println!("state_dir\t{}", status.state_dir.display());
    println!("cache_dir\t{}", status.cache_dir.display());
    println!("manifest\t{}", status.manifest_path.display());
    println!("library\t{}", status.library_count);
    println!("grid\t{}x{}", status.columns, status.rows);
    println!("fingerprint\t{:016x}", status.fingerprint);
    println!("order_policy\t{}", status.order_policy);
    println!("thumbnail_edge\t{}", status.thumbnail_edge);
    println!("cell_ready\t{}", status.cell_ready);
    println!("cell_missing\t{}", status.cell_missing);
    println!("world_tile_ready\t{}", status.world_tile_ready);
    println!("world_tile_missing\t{}", status.world_tile_missing);
    for lod in &status.world_lods {
        println!(
            "world_tiles_l{}\tready\t{}\tmissing\t{}",
            lod.lod, lod.tile_ready, lod.tile_missing
        );
    }
    println!("last_compute_failed\t{}", world_cache_failure_count(status));
    println!("failure_log\t{}", failure_log.display());
    println!("manifest_state\t{}", status.manifest_state.as_str());
    println!("ready\t{}", status.ready);
    println!("message\t{}", status.message);
}

fn print_world_cache_failures(
    status: &WorldCacheStatus,
    failures: &[WorldCacheFailureRecord],
    json: bool,
) {
    if json {
        println!(
            "{{\"failure_log\":{},\"failure_count\":{},\"failures\":{}}}",
            json_string(&world_cache_failure_log_path(status).display().to_string()),
            failures.len(),
            encode_world_cache_failures(failures)
        );
        return;
    }

    for failure in failures {
        println!("{}\t{}\t{}", failure.kind, failure.item, failure.message);
    }
}

fn print_world_cache_plan(plan: &WorldCachePlan, progress: bool, json: bool) {
    if json {
        println!(
            "{{\"status\":{},\"thumbnail_count\":{},\"thumbnail_ready\":{},\"thumbnail_missing\":{},\"world_tile_count\":{},\"world_tile_ready\":{},\"world_tile_missing\":{},\"planned_world_lods\":{},\"estimated_remaining_bytes\":{},\"dry_run\":{}}}",
            encode_world_cache_status(&plan.status),
            plan.thumbnail_count,
            plan.thumbnail_ready,
            plan.thumbnail_missing,
            plan.world_tile_count,
            plan.world_tile_ready,
            plan.world_tile_missing,
            encode_world_lod_plans(&plan.world_lods),
            plan.estimated_remaining_bytes,
            plan.dry_run
        );
        return;
    }

    if progress {
        println!("indexed {} wallpapers", plan.status.library_count);
        println!(
            "cell thumbnails {}/{} ready; {} planned",
            plan.thumbnail_ready, plan.thumbnail_count, plan.thumbnail_missing
        );
        print_world_lod_plan_progress(&plan.world_lods);
        println!(
            "estimated remaining {} ({} bytes)",
            format_bytes(plan.estimated_remaining_bytes),
            plan.estimated_remaining_bytes
        );
    } else {
        println!("library\t{}", plan.status.library_count);
        println!("cell_thumbnails_planned\t{}", plan.thumbnail_count);
        println!("cell_thumbnails_ready\t{}", plan.thumbnail_ready);
        println!("cell_thumbnails_missing\t{}", plan.thumbnail_missing);
        println!("world_tiles_planned\t{}", plan.world_tile_count);
        println!("world_tiles_ready\t{}", plan.world_tile_ready);
        println!("world_tiles_missing\t{}", plan.world_tile_missing);
        println!(
            "estimated_remaining_bytes\t{}",
            plan.estimated_remaining_bytes
        );
        for lod in &plan.world_lods {
            println!("world_tiles_l{}_planned\t{}", lod.lod, lod.tile_count);
            println!("world_tiles_l{}_ready\t{}", lod.lod, lod.tile_ready);
            println!("world_tiles_l{}_missing\t{}", lod.lod, lod.tile_missing);
        }
    }
    if plan.dry_run {
        println!("dry_run\ttrue");
    }
    println!("message\tworld tile LOD generation planned from real cached imagery");
}

fn print_world_lod_plan_progress(lods: &[WorldLodPlan]) {
    if lods.is_empty() {
        println!("world tiles 0 planned");
        return;
    }
    for lod in lods {
        println!(
            "world tiles L{} {}/{} ready; {} planned",
            lod.lod, lod.tile_ready, lod.tile_count, lod.tile_missing
        );
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct WorldCacheComputeResult {
    cell_generated: usize,
    cell_skipped: usize,
    tile_generated: usize,
    tile_skipped: usize,
    failed: usize,
    limited: bool,
}

struct WorldCacheFailure {
    kind: &'static str,
    item: String,
    message: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct WorldCacheFailureRecord {
    kind: String,
    item: String,
    message: String,
}

#[derive(Debug)]
struct WorldCachePlan {
    status: WorldCacheStatus,
    thumbnail_count: usize,
    thumbnail_ready: usize,
    thumbnail_missing: usize,
    world_tile_count: usize,
    world_tile_ready: usize,
    world_tile_missing: usize,
    world_lods: Vec<WorldLodPlan>,
    estimated_remaining_bytes: u64,
    dry_run: bool,
}

#[derive(Debug)]
struct WorldCacheSelection {
    status: WorldCacheStatus,
    cell_entries: Vec<WorldCellCacheEntry>,
    tile_entries: Vec<WorldTileCacheEntry>,
    cell_indices: Vec<usize>,
    limited: bool,
}

fn plan_world_cache_compute_for_scope(
    config: &MuralConfig,
    scope: &WorldCacheComputeScope,
    limit: Option<usize>,
    tile_limit: Option<usize>,
    dry_run: bool,
) -> Result<WorldCachePlan, String> {
    let selection = world_cache_selection(config, scope, limit, tile_limit, false)?;
    let thumbnail_ready = selected_world_cell_ready_count(&selection);
    let thumbnail_count = selection.cell_indices.len();
    let world_tile_ready = selection
        .tile_entries
        .iter()
        .filter(|entry| entry.image_path.is_file())
        .count();
    let world_tile_count = selection.tile_entries.len();
    Ok(WorldCachePlan {
        thumbnail_count,
        thumbnail_ready,
        thumbnail_missing: thumbnail_count.saturating_sub(thumbnail_ready),
        world_tile_count,
        world_tile_ready,
        world_tile_missing: world_tile_count.saturating_sub(world_tile_ready),
        world_lods: world_lod_plan_counts(&selection.tile_entries),
        estimated_remaining_bytes: estimated_world_cache_remaining_bytes(&selection),
        status: selection.status,
        dry_run,
    })
}

fn selected_world_cell_ready_count(selection: &WorldCacheSelection) -> usize {
    selection
        .cell_indices
        .iter()
        .filter(|index| {
            selection
                .cell_entries
                .get(**index)
                .is_some_and(|entry| entry.image_path.is_file())
        })
        .count()
}

fn world_lod_plan_counts(tile_entries: &[WorldTileCacheEntry]) -> Vec<WorldLodPlan> {
    let mut counts = BTreeMap::new();
    for tile in tile_entries {
        let (tile_count, tile_ready) = counts.entry(tile.lod).or_insert((0, 0));
        *tile_count += 1;
        if tile.image_path.is_file() {
            *tile_ready += 1;
        }
    }
    counts
        .into_iter()
        .map(|(lod, (tile_count, tile_ready))| WorldLodPlan {
            lod,
            tile_count,
            tile_ready,
            tile_missing: tile_count.saturating_sub(tile_ready),
        })
        .collect()
}

fn estimated_world_cache_remaining_bytes(selection: &WorldCacheSelection) -> u64 {
    estimate_missing_cell_bytes(&selection.cell_entries, &selection.cell_indices)
        .saturating_add(estimate_missing_tile_bytes(&selection.tile_entries))
}

fn estimate_missing_cell_bytes(cells: &[WorldCellCacheEntry], indices: &[usize]) -> u64 {
    let mut ready_count = 0_u64;
    let mut ready_bytes = 0_u64;
    let mut missing_count = 0_u64;

    for index in indices {
        let Some(cell) = cells.get(*index) else {
            continue;
        };
        match fs::metadata(&cell.image_path) {
            Ok(metadata) if metadata.is_file() => {
                ready_count = ready_count.saturating_add(1);
                ready_bytes = ready_bytes.saturating_add(metadata.len());
            }
            _ => missing_count = missing_count.saturating_add(1),
        }
    }

    let fallback = uncompressed_pixels_bytes(
        u64::from(DEFAULT_WORLD_CELL_THUMBNAIL_EDGE),
        u64::from(DEFAULT_WORLD_CELL_THUMBNAIL_EDGE),
    );
    let average = average_or_fallback(ready_bytes, ready_count, fallback);
    missing_count.saturating_mul(average)
}

fn estimate_missing_tile_bytes(tiles: &[WorldTileCacheEntry]) -> u64 {
    let mut ready_by_lod: BTreeMap<usize, (u64, u64)> = BTreeMap::new();
    for tile in tiles {
        match fs::metadata(&tile.image_path) {
            Ok(metadata) if metadata.is_file() => {
                let (count, bytes) = ready_by_lod.entry(tile.lod).or_insert((0, 0));
                *count = count.saturating_add(1);
                *bytes = bytes.saturating_add(metadata.len());
            }
            _ => {}
        }
    }

    tiles
        .iter()
        .filter(|tile| !tile.image_path.is_file())
        .map(|tile| {
            let (ready_count, ready_bytes) = ready_by_lod.get(&tile.lod).copied().unwrap_or((0, 0));
            average_or_fallback(
                ready_bytes,
                ready_count,
                estimated_world_tile_uncompressed_bytes(tile),
            )
        })
        .fold(0_u64, u64::saturating_add)
}

fn average_or_fallback(bytes: u64, count: u64, fallback: u64) -> u64 {
    if count == 0 {
        return fallback;
    }
    bytes.div_ceil(count).max(1)
}

fn estimated_world_tile_uncompressed_bytes(tile: &WorldTileCacheEntry) -> u64 {
    let edge = u64::from(DEFAULT_WORLD_CELL_THUMBNAIL_EDGE);
    let (width_units, height_units) = estimated_world_tile_pixel_units(tile);
    uncompressed_pixels_bytes(
        width_units.saturating_mul(edge),
        height_units.saturating_mul(edge),
    )
}

fn estimated_world_tile_pixel_units(tile: &WorldTileCacheEntry) -> (u64, u64) {
    if tile.lod == 0 {
        return (
            tile.end_column.saturating_sub(tile.start_column).max(1) as u64,
            tile.end_row.saturating_sub(tile.start_row).max(1) as u64,
        );
    }

    let child_cells = world_lod_tile_cells(DEFAULT_WORLD_TILE_CELLS, tile.lod.saturating_sub(1));
    let start_child_column = tile.start_column / child_cells;
    let end_child_column = tile.end_column.div_ceil(child_cells);
    let start_child_row = tile.start_row / child_cells;
    let end_child_row = tile.end_row.div_ceil(child_cells);
    (
        end_child_column.saturating_sub(start_child_column).max(1) as u64,
        end_child_row.saturating_sub(start_child_row).max(1) as u64,
    )
}

fn uncompressed_pixels_bytes(width: u64, height: u64) -> u64 {
    width.saturating_mul(height).saturating_mul(4)
}

fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut unit_index = 0_usize;
    let mut divisor = 1_u64;
    while bytes / divisor >= 1024 && unit_index < UNITS.len() - 1 {
        divisor = divisor.saturating_mul(1024);
        unit_index += 1;
    }
    if unit_index == 0 {
        return format!("{bytes} B");
    }
    let mut whole = bytes / divisor;
    let mut tenths = ((bytes % divisor).saturating_mul(10) + (divisor / 2)) / divisor;
    if tenths == 10 {
        whole = whole.saturating_add(1);
        tenths = 0;
    }
    format!("{}.{} {}", whole, tenths, UNITS[unit_index])
}

fn format_rate_per_second(processed: usize, elapsed: Duration) -> String {
    let elapsed_ms = elapsed.as_millis().max(1);
    let processed = processed as u128;
    let milli_rate = processed.saturating_mul(1_000_000) / elapsed_ms;
    format!("{}.{:03}", milli_rate / 1_000, milli_rate % 1_000)
}

fn world_cache_failure_log_path(status: &WorldCacheStatus) -> PathBuf {
    status.cache_dir.join(WORLD_CACHE_FAILURE_LOG)
}

fn world_cache_failure_count(status: &WorldCacheStatus) -> usize {
    match fs::read_to_string(world_cache_failure_log_path(status)) {
        Ok(content) => failure_log_entry_count(&content),
        Err(_) => 0,
    }
}

fn failure_log_entry_count(content: &str) -> usize {
    content
        .lines()
        .filter(|line| !line.trim().is_empty())
        .count()
}

fn read_world_cache_failure_records(
    status: &WorldCacheStatus,
) -> Result<Vec<WorldCacheFailureRecord>, String> {
    let path = world_cache_failure_log_path(status);
    match fs::read_to_string(&path) {
        Ok(content) => Ok(parse_world_cache_failure_records(&content)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(format!("failed to read {}: {error}", path.display())),
    }
}

fn parse_world_cache_failure_records(content: &str) -> Vec<WorldCacheFailureRecord> {
    content
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let mut fields = line.splitn(3, '\t');
            WorldCacheFailureRecord {
                kind: fields.next().unwrap_or_default().to_owned(),
                item: fields.next().unwrap_or_default().to_owned(),
                message: fields.next().unwrap_or_default().to_owned(),
            }
        })
        .collect()
}

fn write_world_cache_failure_log(
    status: &WorldCacheStatus,
    failures: &[WorldCacheFailure],
) -> Result<(), String> {
    fs::create_dir_all(&status.cache_dir).map_err(|error| {
        format!(
            "failed to create world cache directory {}: {error}",
            status.cache_dir.display()
        )
    })?;
    let path = world_cache_failure_log_path(status);
    let tmp = path.with_extension("tmp");
    let content = failures
        .iter()
        .map(format_world_cache_failure)
        .collect::<Vec<_>>()
        .join("\n");
    let content = if content.is_empty() {
        content
    } else {
        format!("{content}\n")
    };
    fs::write(&tmp, content)
        .map_err(|error| format!("failed to write {}: {error}", tmp.display()))?;
    fs::rename(&tmp, &path)
        .map_err(|error| format!("failed to replace {}: {error}", path.display()))
}

fn format_world_cache_failure(failure: &WorldCacheFailure) -> String {
    format!(
        "{}\t{}\t{}",
        sanitize_failure_field(failure.kind),
        sanitize_failure_field(&failure.item),
        sanitize_failure_field(&failure.message)
    )
}

fn sanitize_failure_field(field: &str) -> String {
    field
        .chars()
        .map(|character| match character {
            '\t' | '\n' | '\r' => ' ',
            _ => character,
        })
        .collect()
}

#[allow(clippy::too_many_lines)]
fn compute_world_cache(
    config: &MuralConfig,
    scope: &WorldCacheComputeScope,
    limit: Option<usize>,
    tile_limit: Option<usize>,
    workers: usize,
    progress: bool,
) -> Result<WorldCacheComputeResult, String> {
    let started_at = Instant::now();
    let selection = world_cache_selection(config, scope, limit, tile_limit, true)?;
    if progress {
        println!("indexed {} wallpapers", selection.status.library_count);
        println!(
            "cell thumbnails starting 0/{}{}",
            selection.cell_indices.len(),
            if selection.limited { " limited" } else { "" }
        );
        if !selection.tile_entries.is_empty() {
            for lod in world_lod_plan_counts(&selection.tile_entries) {
                println!("world tiles L{} starting 0/{}", lod.lod, lod.tile_count);
            }
        }
        let estimated_remaining_bytes = estimated_world_cache_remaining_bytes(&selection);
        println!(
            "estimated remaining {} ({} bytes)",
            format_bytes(estimated_remaining_bytes),
            estimated_remaining_bytes
        );
    }

    let mut result = WorldCacheComputeResult {
        cell_generated: 0,
        cell_skipped: 0,
        tile_generated: 0,
        tile_skipped: 0,
        failed: 0,
        limited: selection.limited,
    };
    let mut failures = Vec::new();
    for (position, cell_index) in selection.cell_indices.iter().copied().enumerate() {
        let Some(entry) = selection.cell_entries.get(cell_index) else {
            continue;
        };
        if entry.image_path.is_file() {
            result.cell_skipped += 1;
        } else {
            if let Some(parent) = entry.image_path.parent() {
                fs::create_dir_all(parent).map_err(|error| {
                    format!(
                        "failed to create world cell cache directory {}: {error}",
                        parent.display()
                    )
                })?;
            }
            match generate_world_cell_thumbnail(
                &entry.source_path,
                &entry.image_path,
                DEFAULT_WORLD_CELL_THUMBNAIL_EDGE,
            ) {
                Ok(()) => result.cell_generated += 1,
                Err(message) => {
                    result.failed += 1;
                    eprintln!(
                        "muralctl: failed world cell thumbnail {}: {message}",
                        entry.source_path
                    );
                    failures.push(WorldCacheFailure {
                        kind: "cell",
                        item: entry.source_path.clone(),
                        message,
                    });
                }
            }
        }
        if progress {
            println!(
                "cell thumbnails {}/{} generated={} skipped={} failed={}",
                position + 1,
                selection.cell_indices.len(),
                result.cell_generated,
                result.cell_skipped,
                result.failed
            );
        }
    }

    for (position, tile) in selection.tile_entries.iter().enumerate() {
        if tile.image_path.is_file() {
            result.tile_skipped += 1;
        } else {
            if let Some(parent) = tile.image_path.parent() {
                fs::create_dir_all(parent).map_err(|error| {
                    format!(
                        "failed to create world tile cache directory {}: {error}",
                        parent.display()
                    )
                })?;
            }
            match generate_world_tile(
                tile,
                &selection.cell_entries,
                selection.status.columns,
                workers,
            ) {
                Ok(tile_failures) => {
                    result.tile_generated += 1;
                    result.failed += tile_failures.len();
                    for failure in tile_failures {
                        eprintln!(
                            "muralctl: skipped world cell thumbnail {}: {}",
                            failure.item, failure.message
                        );
                        failures.push(failure);
                    }
                }
                Err(message) => {
                    result.failed += 1;
                    eprintln!(
                        "muralctl: failed world tile l{} r{} c{}: {message}",
                        tile.lod, tile.tile_row, tile.tile_column
                    );
                    failures.push(WorldCacheFailure {
                        kind: "tile",
                        item: format!(
                            "l{} r{} c{} {}",
                            tile.lod,
                            tile.tile_row,
                            tile.tile_column,
                            tile.image_path.display()
                        ),
                        message,
                    });
                }
            }
        }
        if progress {
            println!(
                "world tiles {}/{} L{} generated={} skipped={} failed={}",
                position + 1,
                selection.tile_entries.len(),
                tile.lod,
                result.tile_generated,
                result.tile_skipped,
                result.failed
            );
        }
    }

    write_world_cache_failure_log(&selection.status, &failures)?;
    if progress {
        let elapsed = started_at.elapsed();
        let processed = selection
            .cell_indices
            .len()
            .saturating_add(selection.tile_entries.len());
        println!("elapsed_ms\t{}", elapsed.as_millis());
        println!(
            "work_rate_per_sec\t{}",
            format_rate_per_second(processed, elapsed)
        );
    }
    Ok(result)
}

fn world_cache_selection(
    config: &MuralConfig,
    scope: &WorldCacheComputeScope,
    limit: Option<usize>,
    tile_limit: Option<usize>,
    write_index: bool,
) -> Result<WorldCacheSelection, String> {
    let status = if write_index {
        write_world_cache_index(config)?
    } else {
        world_cache_status(config)?
    };
    let cell_entries = world_cell_cache_entries(config, DEFAULT_WORLD_CELL_THUMBNAIL_EDGE)?;
    let mut tile_entries = world_tile_pyramid_cache_entries(
        config,
        DEFAULT_WORLD_CELL_THUMBNAIL_EDGE,
        DEFAULT_WORLD_TILE_CELLS,
    )?;
    if let WorldCacheComputeScope::Route { routes } = scope {
        tile_entries = route_world_tile_entries(
            &cell_entries,
            status.columns,
            &status.world_lods,
            tile_entries,
            routes,
        )?;
    } else if let WorldCacheComputeScope::Neighborhood {
        centers,
        radius,
        lod,
    } = scope
    {
        tile_entries = neighborhood_world_tile_entries(
            &cell_entries,
            status.columns,
            status.rows,
            &status.world_lods,
            tile_entries,
            centers,
            *radius,
            *lod,
        )?;
    }

    let generate_tiles = tile_limit.is_some() || limit.is_none();
    let tile_limited = tile_limit.is_some_and(|limit| limit < tile_entries.len());
    if let Some(limit) = tile_limit {
        tile_entries.truncate(limit);
    }
    if !generate_tiles {
        tile_entries.clear();
    }
    let cell_indices = selected_world_cell_indices_for_scope(
        scope,
        cell_entries.len(),
        status.columns,
        limit,
        tile_limit,
        &tile_entries,
    );
    let cell_limited = matches!(scope, WorldCacheComputeScope::All)
        && limit.is_some_and(|limit| limit < cell_entries.len());

    Ok(WorldCacheSelection {
        status,
        cell_entries,
        tile_entries,
        cell_indices,
        limited: cell_limited || tile_limited,
    })
}

fn route_world_tile_entries(
    cells: &[WorldCellCacheEntry],
    columns: usize,
    lods: &[WorldLodCacheStatus],
    tiles: Vec<WorldTileCacheEntry>,
    routes: &[WorldCacheRoute],
) -> Result<Vec<WorldTileCacheEntry>, String> {
    let needed = route_world_tile_keys(cells, columns, lods, routes)?;

    Ok(tiles
        .into_iter()
        .filter(|tile| needed.contains(&(tile.lod, tile.tile_row, tile.tile_column)))
        .collect())
}

#[allow(clippy::too_many_arguments)]
fn neighborhood_world_tile_entries(
    cells: &[WorldCellCacheEntry],
    columns: usize,
    rows: usize,
    lods: &[WorldLodCacheStatus],
    tiles: Vec<WorldTileCacheEntry>,
    centers: &[String],
    radius: usize,
    lod: usize,
) -> Result<Vec<WorldTileCacheEntry>, String> {
    let needed = neighborhood_world_tile_keys(cells, columns, rows, lods, centers, radius, lod)?;

    Ok(tiles
        .into_iter()
        .filter(|tile| needed.contains(&(tile.lod, tile.tile_row, tile.tile_column)))
        .collect())
}

fn route_world_tile_keys(
    cells: &[WorldCellCacheEntry],
    columns: usize,
    lods: &[WorldLodCacheStatus],
    routes: &[WorldCacheRoute],
) -> Result<BTreeSet<(usize, usize, usize)>, String> {
    let mut needed = BTreeSet::new();
    for route in routes {
        needed.extend(route_world_tile_keys_for_pair(
            cells,
            columns,
            lods,
            &route.from,
            &route.to,
        )?);
    }
    Ok(needed)
}

fn neighborhood_world_tile_keys(
    cells: &[WorldCellCacheEntry],
    columns: usize,
    rows: usize,
    lods: &[WorldLodCacheStatus],
    centers: &[String],
    radius: usize,
    lod: usize,
) -> Result<BTreeSet<(usize, usize, usize)>, String> {
    if !lods.iter().any(|entry| entry.lod == lod) {
        return Err(format!("world cache neighborhood LOD {lod} is not indexed"));
    }

    let mut needed = BTreeSet::new();
    for center in centers {
        needed.extend(neighborhood_world_tile_keys_for_center(
            cells, columns, rows, center, radius, lod,
        )?);
    }
    Ok(needed)
}

fn neighborhood_world_tile_keys_for_center(
    cells: &[WorldCellCacheEntry],
    columns: usize,
    rows: usize,
    center: &str,
    radius: usize,
    lod: usize,
) -> Result<BTreeSet<(usize, usize, usize)>, String> {
    let center_index = world_cell_index(cells, center).ok_or_else(|| {
        format!("world cache neighborhood --center path is not in the library: {center}")
    })?;
    let tile_cells = world_lod_tile_cells(DEFAULT_WORLD_TILE_CELLS, lod);
    let tile_columns = columns.div_ceil(tile_cells);
    let tile_rows = rows.div_ceil(tile_cells);
    if tile_columns == 0 || tile_rows == 0 {
        return Ok(BTreeSet::new());
    }

    let center_column = center_index % columns;
    let center_row = center_index / columns;
    let center_tile_column = center_column / tile_cells;
    let center_tile_row = center_row / tile_cells;
    let start_column = center_tile_column.saturating_sub(radius);
    let start_row = center_tile_row.saturating_sub(radius);
    let end_column = center_tile_column
        .saturating_add(radius)
        .min(tile_columns.saturating_sub(1));
    let end_row = center_tile_row
        .saturating_add(radius)
        .min(tile_rows.saturating_sub(1));

    let mut needed = BTreeSet::new();
    for tile_row in start_row..=end_row {
        for tile_column in start_column..=end_column {
            needed.insert((lod, tile_row, tile_column));
        }
    }
    Ok(needed)
}

fn route_world_tile_keys_for_pair(
    cells: &[WorldCellCacheEntry],
    columns: usize,
    lods: &[WorldLodCacheStatus],
    from: &str,
    to: &str,
) -> Result<BTreeSet<(usize, usize, usize)>, String> {
    let from_index = world_cell_index(cells, from)
        .ok_or_else(|| format!("world cache route --from path is not in the library: {from}"))?;
    let to_index = world_cell_index(cells, to)
        .ok_or_else(|| format!("world cache route --to path is not in the library: {to}"))?;
    let layout = WorldLayout::new(cells.len(), columns);
    let lod = select_world_route_lod(lods, layout, from_index, to_index)?;
    Ok(route_tile_keys(layout, from_index, to_index, lod))
}

fn select_world_route_lod(
    lods: &[WorldLodCacheStatus],
    layout: WorldLayout,
    from_index: usize,
    to_index: usize,
) -> Result<usize, String> {
    for lod in lods {
        let tile_count = world_tiles_for_route(
            layout,
            from_index,
            to_index,
            world_lod_tile_cells(DEFAULT_WORLD_TILE_CELLS, lod.lod),
            1.0,
        )
        .len();
        if tile_count <= MAX_WORLD_ROUTE_TILES {
            return Ok(lod.lod);
        }
    }

    Err(format!(
        "world cache route exceeds {MAX_WORLD_ROUTE_TILES} tile(s) at every available LOD"
    ))
}

fn route_tile_keys(
    layout: WorldLayout,
    from_index: usize,
    to_index: usize,
    lod: usize,
) -> BTreeSet<(usize, usize, usize)> {
    world_tiles_for_route(
        layout,
        from_index,
        to_index,
        world_lod_tile_cells(DEFAULT_WORLD_TILE_CELLS, lod),
        1.0,
    )
    .into_iter()
    .map(|tile| (lod, tile.row, tile.column))
    .collect()
}

fn world_cell_index(cells: &[WorldCellCacheEntry], source_path: &str) -> Option<usize> {
    cells
        .iter()
        .position(|entry| entry.source_path == source_path)
}

fn selected_world_cell_indices_for_scope(
    scope: &WorldCacheComputeScope,
    cell_count: usize,
    columns: usize,
    limit: Option<usize>,
    tile_limit: Option<usize>,
    tiles: &[WorldTileCacheEntry],
) -> Vec<usize> {
    if !matches!(scope, WorldCacheComputeScope::All) {
        return Vec::new();
    }

    if limit.is_none() && tile_limit.is_none() {
        return (0..cell_count).collect();
    }
    selected_world_cell_indices(cell_count, columns, limit, tiles)
}

fn selected_world_cell_indices(
    cell_count: usize,
    columns: usize,
    limit: Option<usize>,
    tiles: &[WorldTileCacheEntry],
) -> Vec<usize> {
    let mut indices = BTreeSet::new();
    if let Some(limit) = limit {
        indices.extend(0..limit.min(cell_count));
    }
    for tile in tiles {
        if tile.image_path.is_file() {
            continue;
        }
        for row in tile.start_row..tile.end_row {
            for column in tile.start_column..tile.end_column {
                let index = row.saturating_mul(columns).saturating_add(column);
                if index < cell_count {
                    indices.insert(index);
                }
            }
        }
    }
    indices.into_iter().collect()
}

fn generate_world_cell_thumbnail(
    source_path: &str,
    image_path: &Path,
    edge: u32,
) -> Result<(), String> {
    let thumbnail = generate_world_cell_thumbnail_image(source_path, edge, edge)?;
    let tmp = image_path.with_extension("tmp");
    thumbnail
        .save_with_format(&tmp, ImageFormat::Png)
        .map_err(|error| format!("failed to write thumbnail {}: {error}", tmp.display()))?;
    fs::rename(&tmp, image_path).map_err(|error| {
        format!(
            "failed to replace thumbnail {}: {error}",
            image_path.display()
        )
    })
}

fn generate_world_tile(
    tile: &WorldTileCacheEntry,
    cells: &[mural_core::world_cache::WorldCellCacheEntry],
    columns: usize,
    workers: usize,
) -> Result<Vec<WorldCacheFailure>, String> {
    if tile.lod > 0 {
        return generate_world_lod_tile(tile, cells, columns, workers);
    }

    generate_world_l0_tile(tile, cells, columns)
}

fn generate_world_l0_tile(
    tile: &WorldTileCacheEntry,
    cells: &[mural_core::world_cache::WorldCellCacheEntry],
    columns: usize,
) -> Result<Vec<WorldCacheFailure>, String> {
    let width_cells = tile.end_column.saturating_sub(tile.start_column);
    let height_cells = tile.end_row.saturating_sub(tile.start_row);
    let edge = DEFAULT_WORLD_CELL_THUMBNAIL_EDGE;
    let mut canvas = RgbaImage::from_pixel(
        tile_pixels(width_cells, edge, "tile width")?,
        tile_pixels(height_cells, edge, "tile height")?,
        Rgba([0, 0, 0, 0]),
    );
    let mut failures = Vec::new();

    for row in tile.start_row..tile.end_row {
        for column in tile.start_column..tile.end_column {
            let index = row.saturating_mul(columns).saturating_add(column);
            let Some(cell) = cells.get(index) else {
                continue;
            };
            let image = match load_or_generate_world_cell_thumbnail(cell, edge, edge) {
                Ok(image) => image,
                Err(message) => {
                    failures.push(WorldCacheFailure {
                        kind: "cell",
                        item: cell.source_path.clone(),
                        message,
                    });
                    continue;
                }
            };
            let x = tile_pixels(column - tile.start_column, edge, "tile x offset")?;
            let y = tile_pixels(row - tile.start_row, edge, "tile y offset")?;
            image::imageops::overlay(&mut canvas, &image, i64::from(x), i64::from(y));
        }
    }

    let tmp = tile.image_path.with_extension("tmp");
    canvas
        .save_with_format(&tmp, ImageFormat::Png)
        .map_err(|error| format!("failed to write world tile {}: {error}", tmp.display()))?;
    fs::rename(&tmp, &tile.image_path).map_err(|error| {
        format!(
            "failed to replace world tile {}: {error}",
            tile.image_path.display()
        )
    })?;
    Ok(failures)
}

fn generate_world_lod_tile(
    tile: &WorldTileCacheEntry,
    cells: &[mural_core::world_cache::WorldCellCacheEntry],
    columns: usize,
    workers: usize,
) -> Result<Vec<WorldCacheFailure>, String> {
    let child_lod = tile.lod.saturating_sub(1);
    let child_world_cells =
        mural_core::world_cache::world_lod_tile_cells(DEFAULT_WORLD_TILE_CELLS, child_lod);
    let start_child_column = tile.start_column / child_world_cells;
    let end_child_column = tile.end_column.div_ceil(child_world_cells);
    let start_child_row = tile.start_row / child_world_cells;
    let end_child_row = tile.end_row.div_ceil(child_world_cells);
    let width_children = end_child_column.saturating_sub(start_child_column);
    let height_children = end_child_row.saturating_sub(start_child_row);
    let edge = DEFAULT_WORLD_CELL_THUMBNAIL_EDGE;
    let mut canvas = RgbaImage::from_pixel(
        tile_pixels(width_children, edge, "tile width")?,
        tile_pixels(height_children, edge, "tile height")?,
        Rgba([0, 0, 0, 0]),
    );
    let mut failures = Vec::new();

    let child_blocks = world_lod_child_blocks(
        start_child_column,
        end_child_column,
        start_child_row,
        end_child_row,
        child_world_cells,
        tile,
    );
    for (block, generated) in
        generate_world_lod_child_blocks(&child_blocks, cells, columns, edge, workers)?
    {
        let x = tile_pixels(
            block.child_column - start_child_column,
            edge,
            "tile x offset",
        )?;
        let y = tile_pixels(block.child_row - start_child_row, edge, "tile y offset")?;
        failures.extend(generated.failures);
        image::imageops::overlay(&mut canvas, &generated.image, i64::from(x), i64::from(y));
    }

    let tmp = tile.image_path.with_extension("tmp");
    canvas
        .save_with_format(&tmp, ImageFormat::Png)
        .map_err(|error| format!("failed to write world tile {}: {error}", tmp.display()))?;
    fs::rename(&tmp, &tile.image_path).map_err(|error| {
        format!(
            "failed to replace world tile {}: {error}",
            tile.image_path.display()
        )
    })?;
    Ok(failures)
}

#[derive(Clone, Copy, Debug)]
struct WorldLodChildBlock {
    child_column: usize,
    child_row: usize,
    start_column: usize,
    end_column: usize,
    start_row: usize,
    end_row: usize,
}

fn world_lod_child_blocks(
    start_child_column: usize,
    end_child_column: usize,
    start_child_row: usize,
    end_child_row: usize,
    child_world_cells: usize,
    tile: &WorldTileCacheEntry,
) -> Vec<WorldLodChildBlock> {
    let mut blocks = Vec::new();
    for child_row in start_child_row..end_child_row {
        for child_column in start_child_column..end_child_column {
            let start_column = child_column.saturating_mul(child_world_cells);
            let start_row = child_row.saturating_mul(child_world_cells);
            let end_column = start_column
                .saturating_add(child_world_cells)
                .min(tile.end_column);
            let end_row = start_row
                .saturating_add(child_world_cells)
                .min(tile.end_row);
            blocks.push(WorldLodChildBlock {
                child_column,
                child_row,
                start_column,
                end_column,
                start_row,
                end_row,
            });
        }
    }
    blocks
}

fn generate_world_lod_child_blocks(
    blocks: &[WorldLodChildBlock],
    cells: &[mural_core::world_cache::WorldCellCacheEntry],
    columns: usize,
    edge: u32,
    workers: usize,
) -> Result<Vec<(WorldLodChildBlock, WorldCellBlock)>, String> {
    let worker_count = workers.max(1);
    if worker_count == 1 || blocks.len() <= 1 {
        return Ok(blocks
            .iter()
            .copied()
            .map(|block| {
                let image = generate_world_lod_child_block(block, cells, columns, edge);
                (block, image)
            })
            .collect());
    }

    let mut generated = Vec::with_capacity(blocks.len());
    for chunk in blocks.chunks(worker_count) {
        let results = thread::scope(|scope| {
            let handles = chunk
                .iter()
                .copied()
                .map(|block| {
                    scope.spawn(move || {
                        let image = generate_world_lod_child_block(block, cells, columns, edge);
                        (block, image)
                    })
                })
                .collect::<Vec<_>>();
            handles
                .into_iter()
                .map(|handle| {
                    handle
                        .join()
                        .map_err(|_| "world tile worker panicked".to_owned())
                })
                .collect::<Result<Vec<_>, String>>()
        })?;
        generated.extend(results);
    }
    Ok(generated)
}

fn generate_world_lod_child_block(
    block: WorldLodChildBlock,
    cells: &[mural_core::world_cache::WorldCellCacheEntry],
    columns: usize,
    edge: u32,
) -> WorldCellBlock {
    generate_world_cell_block(
        cells,
        columns,
        block.start_column,
        block.end_column,
        block.start_row,
        block.end_row,
        edge,
    )
}

struct WorldCellBlock {
    image: RgbaImage,
    failures: Vec<WorldCacheFailure>,
}

fn generate_world_cell_block(
    cells: &[mural_core::world_cache::WorldCellCacheEntry],
    columns: usize,
    start_column: usize,
    end_column: usize,
    start_row: usize,
    end_row: usize,
    edge: u32,
) -> WorldCellBlock {
    let width_cells = end_column.saturating_sub(start_column).max(1);
    let height_cells = end_row.saturating_sub(start_row).max(1);
    let mut canvas = RgbaImage::from_pixel(edge, edge, Rgba([0, 0, 0, 0]));
    let mut failures = Vec::new();

    for row in start_row..end_row {
        for column in start_column..end_column {
            let index = row.saturating_mul(columns).saturating_add(column);
            let Some(cell) = cells.get(index) else {
                continue;
            };
            let cell_column = column.saturating_sub(start_column);
            let cell_row = row.saturating_sub(start_row);
            let x = scaled_offset(cell_column, width_cells, edge);
            let y = scaled_offset(cell_row, height_cells, edge);
            let next_x = scaled_offset(cell_column + 1, width_cells, edge);
            let next_y = scaled_offset(cell_row + 1, height_cells, edge);
            let width = next_x.saturating_sub(x).max(1);
            let height = next_y.saturating_sub(y).max(1);
            let image = match load_or_generate_world_cell_thumbnail(cell, width, height) {
                Ok(image) => image,
                Err(message) => {
                    failures.push(WorldCacheFailure {
                        kind: "cell",
                        item: cell.source_path.clone(),
                        message,
                    });
                    continue;
                }
            };
            image::imageops::overlay(&mut canvas, &image, i64::from(x), i64::from(y));
        }
    }

    WorldCellBlock {
        image: canvas,
        failures,
    }
}

fn load_or_generate_world_cell_thumbnail(
    cell: &mural_core::world_cache::WorldCellCacheEntry,
    width: u32,
    height: u32,
) -> Result<RgbaImage, String> {
    if cell.image_path.is_file() {
        let image = load_world_cell_thumbnail(cell)?;
        if image.width() == width && image.height() == height {
            return Ok(image);
        }
        return Ok(image::imageops::resize(
            &image,
            width,
            height,
            FilterType::Lanczos3,
        ));
    }

    generate_world_cell_thumbnail_image(&cell.source_path, width, height)
}

fn generate_world_cell_thumbnail_image(
    source_path: &str,
    width: u32,
    height: u32,
) -> Result<RgbaImage, String> {
    generate_world_cell_thumbnail_image_internal(source_path, width, height).or_else(
        |internal_error| {
            generate_world_cell_thumbnail_image_vips(source_path, width, height).map_err(
                |vips_error| {
                    format!("{internal_error}; vipsthumbnail fallback failed: {vips_error}")
                },
            )
        },
    )
}

fn generate_world_cell_thumbnail_image_internal(
    source_path: &str,
    width: u32,
    height: u32,
) -> Result<RgbaImage, String> {
    let image = image::ImageReader::open(source_path)
        .map_err(|error| format!("failed to open source image {source_path}: {error}"))?
        .with_guessed_format()
        .map_err(|error| format!("failed to guess image format {source_path}: {error}"))?
        .decode()
        .map_err(|error| format!("failed to decode source image {source_path}: {error}"))?;
    Ok(image
        .resize_to_fill(width, height, FilterType::Lanczos3)
        .to_rgba8())
}

fn generate_world_cell_thumbnail_image_vips(
    source_path: &str,
    width: u32,
    height: u32,
) -> Result<RgbaImage, String> {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis());
    let counter = WORLD_VIPS_TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let tmp = env::temp_dir().join(format!(
        "mural-world-thumb-{}-{width}x{height}-{millis}-{counter}.png",
        process::id()
    ));
    let status = Command::new("vipsthumbnail")
        .arg(source_path)
        .arg("-s")
        .arg(format!("{width}x{height}"))
        .arg("-m")
        .arg("centre")
        .arg("-o")
        .arg(&tmp)
        .arg("--vips-concurrency=1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|error| format!("failed to run vipsthumbnail: {error}"))?;
    if !status.success() {
        let _ = fs::remove_file(&tmp);
        return Err(format!("vipsthumbnail exited with {status}"));
    }
    let image = image::ImageReader::open(&tmp)
        .map_err(|error| {
            format!(
                "failed to open vipsthumbnail output {}: {error}",
                tmp.display()
            )
        })?
        .with_guessed_format()
        .map_err(|error| {
            format!(
                "failed to guess vipsthumbnail output format {}: {error}",
                tmp.display()
            )
        })?
        .decode()
        .map_err(|error| {
            format!(
                "failed to decode vipsthumbnail output {}: {error}",
                tmp.display()
            )
        })?
        .to_rgba8();
    let _ = fs::remove_file(&tmp);
    Ok(image)
}

fn load_world_cell_thumbnail(
    cell: &mural_core::world_cache::WorldCellCacheEntry,
) -> Result<RgbaImage, String> {
    image::ImageReader::open(&cell.image_path)
        .map_err(|error| {
            format!(
                "failed to open cell thumbnail {}: {error}",
                cell.image_path.display()
            )
        })?
        .with_guessed_format()
        .map_err(|error| {
            format!(
                "failed to guess cell thumbnail format {}: {error}",
                cell.image_path.display()
            )
        })?
        .decode()
        .map_err(|error| {
            format!(
                "failed to decode cell thumbnail {}: {error}",
                cell.image_path.display()
            )
        })
        .map(|image| image.to_rgba8())
}

fn scaled_offset(index: usize, count: usize, edge: u32) -> u32 {
    let count = count.max(1) as u64;
    let index = index as u64;
    index
        .saturating_mul(u64::from(edge))
        .checked_div(count)
        .unwrap_or(0)
        .try_into()
        .unwrap_or(edge)
}

fn tile_pixels(cells: usize, edge: u32, label: &str) -> Result<u32, String> {
    let cells = u32::try_from(cells).map_err(|_| format!("{label} is out of range"))?;
    cells
        .checked_mul(edge)
        .ok_or_else(|| format!("{label} is out of range"))
}

fn encode_world_cache_status(status: &WorldCacheStatus) -> String {
    format!(
        "{{\"wall_dir\":{},\"state_dir\":{},\"cache_dir\":{},\"manifest\":{},\"library\":{},\"columns\":{},\"rows\":{},\"fingerprint\":\"{:016x}\",\"order_policy\":{},\"thumbnail_edge\":{},\"cell_ready\":{},\"cell_missing\":{},\"world_tile_ready\":{},\"world_tile_missing\":{},\"world_lods\":{},\"last_compute_failed\":{},\"failure_log\":{},\"manifest_state\":{},\"ready\":{},\"message\":{}}}",
        json_string(&status.wall_dir.display().to_string()),
        json_string(&status.state_dir.display().to_string()),
        json_string(&status.cache_dir.display().to_string()),
        json_string(&status.manifest_path.display().to_string()),
        status.library_count,
        status.columns,
        status.rows,
        status.fingerprint,
        json_string(&status.order_policy),
        status.thumbnail_edge,
        status.cell_ready,
        status.cell_missing,
        status.world_tile_ready,
        status.world_tile_missing,
        encode_world_lods(status),
        world_cache_failure_count(status),
        json_string(&world_cache_failure_log_path(status).display().to_string()),
        json_string(status.manifest_state.as_str()),
        status.ready,
        json_string(&status.message)
    )
}

fn encode_world_lods(status: &WorldCacheStatus) -> String {
    let mut encoded = String::from("[");
    for (index, lod) in status.world_lods.iter().enumerate() {
        if index > 0 {
            encoded.push(',');
        }
        let _ = write!(
            encoded,
            "{{\"lod\":{},\"tile_ready\":{},\"tile_missing\":{}}}",
            lod.lod, lod.tile_ready, lod.tile_missing
        );
    }
    encoded.push(']');
    encoded
}

fn encode_world_cache_failures(failures: &[WorldCacheFailureRecord]) -> String {
    let mut encoded = String::from("[");
    for (index, failure) in failures.iter().enumerate() {
        if index > 0 {
            encoded.push(',');
        }
        let _ = write!(
            encoded,
            "{{\"kind\":{},\"item\":{},\"message\":{}}}",
            json_string(&failure.kind),
            json_string(&failure.item),
            json_string(&failure.message)
        );
    }
    encoded.push(']');
    encoded
}

fn encode_world_lod_plans(lods: &[WorldLodPlan]) -> String {
    let mut encoded = String::from("[");
    for (index, lod) in lods.iter().enumerate() {
        if index > 0 {
            encoded.push(',');
        }
        let _ = write!(
            encoded,
            "{{\"lod\":{},\"tile_count\":{},\"tile_ready\":{},\"tile_missing\":{}}}",
            lod.lod, lod.tile_count, lod.tile_ready, lod.tile_missing
        );
    }
    encoded.push(']');
    encoded
}

fn json_string(input: &str) -> String {
    let mut encoded = String::with_capacity(input.len() + 2);
    encoded.push('"');
    for character in input.chars() {
        match character {
            '"' => encoded.push_str("\\\""),
            '\\' => encoded.push_str("\\\\"),
            '\n' => encoded.push_str("\\n"),
            '\r' => encoded.push_str("\\r"),
            '\t' => encoded.push_str("\\t"),
            character if character.is_control() => {
                let _ = write!(encoded, "\\u{:04x}", u32::from(character));
            }
            character => encoded.push(character),
        }
    }
    encoded.push('"');
    encoded
}

fn parse_output_path_args(
    args: &[String],
    command_name: &str,
) -> Result<BTreeMap<String, String>, String> {
    let mut outputs = BTreeMap::new();
    let mut pending_output = None;
    let mut index = 0;

    while index < args.len() {
        match args[index].as_str() {
            "-o" | "--output" => {
                parse_output_path_option(args, &mut index, &mut pending_output, &mut outputs)?;
            }
            arg if arg.starts_with('-') => {
                return Err(format!("unknown {command_name} option: {arg}"));
            }
            path => assign_positional_path(path, &mut pending_output, &mut outputs)?,
        }
        index += 1;
    }

    if let Some(output) = pending_output {
        return Err(format!("missing image path for output {output}"));
    }
    if outputs.is_empty() {
        return Err(format!(
            "{command_name} requires at least one --output NAME PATH or --output NAME=PATH"
        ));
    }

    Ok(outputs)
}

fn parse_output_path_option(
    args: &[String],
    index: &mut usize,
    pending_output: &mut Option<String>,
    outputs: &mut BTreeMap<String, String>,
) -> Result<(), String> {
    if let Some(output) = pending_output.as_ref() {
        return Err(format!("missing image path for output {output}"));
    }

    *index += 1;
    let value = args
        .get(*index)
        .ok_or_else(|| "--output requires NAME or NAME=PATH".to_owned())?;

    if let Some((name, path)) = value.split_once('=') {
        insert_output_path(outputs, name, path)?;
        return Ok(());
    }

    if value.is_empty() {
        return Err("output name must not be empty".to_owned());
    }

    if let Some(next) = args.get(*index + 1)
        && !next.starts_with('-')
    {
        *index += 1;
        insert_output_path(outputs, value, next)?;
        return Ok(());
    }

    *pending_output = Some(value.clone());
    Ok(())
}

fn assign_positional_path(
    path: &str,
    pending_output: &mut Option<String>,
    outputs: &mut BTreeMap<String, String>,
) -> Result<(), String> {
    let output = pending_output
        .take()
        .ok_or_else(|| format!("unexpected positional image path: {path}"))?;
    insert_output_path(outputs, &output, path)
}

fn insert_output_path(
    outputs: &mut BTreeMap<String, String>,
    name: &str,
    path: &str,
) -> Result<(), String> {
    if name.is_empty() {
        return Err("output name must not be empty".to_owned());
    }
    if path.is_empty() {
        return Err(format!("image path for output {name} must not be empty"));
    }

    let canonical = canonical_image_path(path)?;
    if outputs.insert(name.to_owned(), canonical).is_some() {
        return Err(format!("duplicate output: {name}"));
    }

    Ok(())
}

fn canonical_image_path(path: &str) -> Result<String, String> {
    let canonical = fs::canonicalize(path)
        .map_err(|error| format!("failed to resolve image path {path}: {error}"))?;
    if !canonical.is_file() {
        return Err(format!("image path is not a file: {}", canonical.display()));
    }

    canonical
        .into_os_string()
        .into_string()
        .map_err(|_| format!("image path is not valid UTF-8: {path}"))
}

fn parse_duration(input: &str) -> Result<u64, String> {
    let duration = input
        .parse::<u64>()
        .map_err(|error| format!("invalid duration {input}: {error}"))?;
    if duration == 0 {
        return Err("duration must be greater than zero".to_owned());
    }
    Ok(duration)
}

fn parse_canvas_tile_count(input: &str) -> Result<CanvasTileCount, String> {
    if input == "auto" {
        return Ok(CanvasTileCount::Auto { max: None });
    }
    parse_canvas_max_tile_count(input).map(CanvasTileCount::Fixed)
}

fn parse_canvas_max_tile_count(input: &str) -> Result<usize, String> {
    let tile_count = input
        .parse::<usize>()
        .map_err(|error| format!("invalid canvas tile count {input}: {error}"))?;
    if tile_count == 0 {
        return Err("canvas tile count must be greater than zero".to_owned());
    }
    if tile_count > MAX_CANVAS_TILE_COUNT {
        return Err(format!(
            "canvas tile count must be at most {MAX_CANVAS_TILE_COUNT}"
        ));
    }
    Ok(tile_count)
}

fn parse_canvas_overview_scale(input: &str) -> Result<f32, String> {
    let scale = input
        .parse::<f32>()
        .map_err(|error| format!("invalid canvas overview scale {input}: {error}"))?;
    if !scale.is_finite() || scale <= 0.0 || scale > 1.0 {
        return Err("canvas overview scale must be greater than 0 and at most 1".to_owned());
    }
    Ok(scale)
}

fn parse_canvas_pan_axis(input: &str) -> Result<CanvasPanAxis, String> {
    CanvasPanAxis::parse(input).map_err(|error| error.to_string())
}

fn is_canvas_transition_token(input: &str) -> bool {
    matches!(
        input,
        "canvas" | "canvas:auto" | "canvas:horizontal" | "canvas:vertical"
    )
}

fn parse_cache_workers(input: &str) -> Result<usize, String> {
    let workers = input
        .parse::<usize>()
        .map_err(|error| format!("invalid cache worker count {input}: {error}"))?;
    if workers == 0 {
        return Err("cache worker count must be greater than zero".to_owned());
    }
    Ok(workers)
}

fn require_no_args(args: &[String], command_name: &str) -> Result<(), String> {
    if args.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "{command_name} does not accept argument: {}",
            args[0]
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mural_ipc::PushDirection;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = env::temp_dir().join(format!("muralctl-world-test-{}-{name}", process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn parses_global_timeout_option() {
        let (socket_path, timeout, filtered) = extract_global_options(args(&[
            "--timeout-ms",
            "2500",
            "--socket",
            "/tmp/mural.sock",
            "ping",
        ]))
        .unwrap();

        assert_eq!(socket_path, Some(PathBuf::from("/tmp/mural.sock")));
        assert_eq!(timeout, Duration::from_millis(2500));
        assert_eq!(filtered, args(&["ping"]));
    }

    #[test]
    fn global_options_defer_default_socket_resolution() {
        let (socket_path, timeout, filtered) =
            extract_global_options(args(&["world", "cache", "status"])).unwrap();

        assert_eq!(socket_path, None);
        assert_eq!(timeout, DEFAULT_TIMEOUT);
        assert_eq!(filtered, args(&["world", "cache", "status"]));
    }

    #[test]
    fn rejects_zero_global_timeout() {
        let error = extract_global_options(args(&["--timeout-ms", "0", "ping"])).unwrap_err();

        assert_eq!(error, "--timeout-ms must be greater than zero");
    }

    #[test]
    fn parses_health_command() {
        let command = build_command(&args(&["health", "--json"]))
            .unwrap()
            .unwrap();

        assert_eq!(command.print_mode, PrintMode::RawJson);
        assert_eq!(command.request, Request::Health);
    }

    #[test]
    fn capabilities_command_defaults_to_human_output() {
        let command = build_command(&args(&["capabilities"])).unwrap().unwrap();

        assert_eq!(command.request, Request::Capabilities);
        assert_eq!(command.print_mode, PrintMode::CapabilitiesText);
    }

    #[test]
    fn capabilities_command_supports_raw_json() {
        let command = build_command(&args(&["capabilities", "--json"]))
            .unwrap()
            .unwrap();

        assert_eq!(command.request, Request::Capabilities);
        assert_eq!(command.print_mode, PrintMode::RawJson);
    }

    #[test]
    fn capabilities_command_rejects_unknown_options() {
        let error = build_command(&args(&["capabilities", "--verbose"])).unwrap_err();

        assert_eq!(error, "capabilities does not accept argument: --verbose");
    }

    #[test]
    fn parses_replace_wallpaper_command() {
        let command = build_command(&args(&["replace", "1"])).unwrap().unwrap();

        assert_eq!(command.print_mode, PrintMode::WallpaperText);
        assert!(matches!(
            command.request,
            Request::Wallpaper(WallpaperRequest {
                action: WallpaperAction::Replace { index: 1 },
                transition: None,
                ..
            })
        ));
    }

    #[test]
    fn high_level_next_without_transition_lets_daemon_use_config() {
        let command = build_command(&args(&["next"])).unwrap().unwrap();

        let Request::Wallpaper(request) = command.request else {
            panic!("expected wallpaper request");
        };
        assert_eq!(request.action, WallpaperAction::Next);
        assert_eq!(request.transition, None);
    }

    #[test]
    fn parses_explicit_push_transition_for_wallpaper_command() {
        let command = build_command(&args(&[
            "next",
            "--transition",
            "push:left",
            "--duration-ms",
            "120",
            "--easing",
            "linear",
            "--mode",
            "pan",
        ]))
        .unwrap()
        .unwrap();

        let Request::Wallpaper(request) = command.request else {
            panic!("expected wallpaper request");
        };
        assert_eq!(request.action, WallpaperAction::Next);
        assert_eq!(
            request.transition,
            Some(Transition::Push {
                direction: PushDirection::Left,
                duration_ms: 120,
                easing: Easing::Linear,
                mode: PushMode::Pan,
            })
        );
    }

    #[test]
    fn parses_explicit_world_transition_for_wallpaper_command() {
        let command = build_command(&args(&[
            "next",
            "--transition",
            "world",
            "--duration-ms",
            "1400",
            "--easing",
            "ease-in-out-cubic",
        ]))
        .unwrap()
        .unwrap();

        let Request::Wallpaper(request) = command.request else {
            panic!("expected wallpaper request");
        };
        assert_eq!(request.action, WallpaperAction::Next);
        assert_eq!(
            request.transition,
            Some(Transition::World {
                duration_ms: 1400,
                easing: Easing::EaseInOutCubic,
            })
        );
    }

    #[test]
    fn parses_shift_back_explicit_duration_with_action_default_transition() {
        let command = build_command(&args(&["shift", "back", "--json", "--duration-ms", "120"]))
            .unwrap()
            .unwrap();

        assert_eq!(command.print_mode, PrintMode::WallpaperJson);
        let Request::Wallpaper(request) = command.request else {
            panic!("expected wallpaper request");
        };
        assert_eq!(request.action, WallpaperAction::ShiftBack);
        assert!(matches!(
            request.transition,
            Some(Transition::Push {
                direction: PushDirection::Right,
                ..
            })
        ));
    }

    #[test]
    fn parses_canvas_wallpaper_options() {
        let command = build_command(&args(&[
            "next",
            "--transition",
            "canvas",
            "--mode",
            "overlap",
            "--canvas-zoom-out-ms",
            "100",
            "--canvas-pan-ms",
            "50",
            "--canvas-zoom-in-ms",
            "200",
            "--canvas-walk",
            "strip",
            "--canvas-pan-axis",
            "horizontal",
            "--canvas-overview-scale",
            "0.25",
            "--canvas-tile-count",
            "auto",
            "--canvas-max-tile-count",
            "9",
        ]))
        .unwrap()
        .unwrap();

        let Request::Wallpaper(request) = command.request else {
            panic!("expected wallpaper request");
        };
        assert_eq!(request.action, WallpaperAction::Next);
        assert_eq!(
            request.transition,
            Some(Transition::Canvas {
                zoom_out_ms: 100,
                pan_ms: 50,
                zoom_in_ms: 200,
                easing: Easing::EaseOutCubic,
                mode: CanvasMode::Overlap,
                walk: CanvasWalk::Strip,
                pan_axis: CanvasPanAxis::Horizontal,
                overview_scale: 0.25,
                tile_count: CanvasTileCount::Auto { max: Some(9) },
            })
        );
    }

    #[test]
    fn parses_canvas_axis_from_transition_token() {
        let command = build_command(&args(&["next", "--transition", "canvas:vertical"]))
            .unwrap()
            .unwrap();

        let Request::Wallpaper(request) = command.request else {
            panic!("expected wallpaper request");
        };
        assert!(matches!(
            request.transition,
            Some(Transition::Canvas {
                pan_axis: CanvasPanAxis::Vertical,
                ..
            })
        ));
    }

    #[test]
    fn rejects_paged_walk_for_canvas_collage_option() {
        for mode in ["collage", "span"] {
            let error = build_command(&args(&["next", "--transition", "canvas", "--mode", mode]))
                .unwrap_err();

            assert!(error.contains("requires canvas walk 'strip'"));
        }
    }

    #[test]
    fn canvas_option_implies_canvas_for_shift_direction() {
        let command = build_command(&args(&["shift", "back", "--canvas-zoom-out-ms", "100"]))
            .unwrap()
            .unwrap();

        let Request::Wallpaper(request) = command.request else {
            panic!("expected wallpaper request");
        };
        assert_eq!(request.action, WallpaperAction::ShiftBack);
        assert!(matches!(
            request.transition,
            Some(Transition::Canvas {
                zoom_out_ms: 100,
                ..
            })
        ));
    }

    #[test]
    fn rejects_canvas_for_low_level_set() {
        let path = env::temp_dir().join(format!("muralctl-canvas-test-{}", process::id()));
        fs::write(&path, b"x").unwrap();
        let output = format!("DP-1={}", path.display());
        let error = build_command(&args(&[
            "set",
            "--output",
            &output,
            "--transition",
            "canvas",
            "--canvas-tile-count",
            "7",
        ]))
        .unwrap_err();

        assert!(error.contains("wallpaper navigation actions"));
    }

    #[test]
    fn parses_world_for_low_level_set() {
        let path = env::temp_dir().join(format!("muralctl-world-test-{}", process::id()));
        fs::write(&path, b"x").unwrap();
        let output = format!("DP-1={}", path.display());
        let command = build_command(&args(&[
            "set",
            "--output",
            &output,
            "--transition",
            "world",
        ]))
        .unwrap()
        .unwrap();

        let Request::Set(request) = command.request else {
            panic!("expected set request");
        };
        assert!(matches!(request.transition, Transition::World { .. }));
    }

    #[test]
    fn parses_cache_warm_command() {
        let command = build_command(&args(&[
            "cache",
            "warm",
            "--scope",
            "all",
            "--workers",
            "8",
            "--backend",
            "vips",
        ]))
        .unwrap()
        .unwrap();

        let Request::Cache(request) = command.request else {
            panic!("expected cache request");
        };
        assert_eq!(
            request.action,
            CacheAction::Warm {
                scope: CacheWarmScope::All,
                workers: 8,
                backend: CacheBackend::Vips,
            }
        );
    }

    #[test]
    fn parses_cache_clear_command() {
        let command = build_command(&args(&["cache", "clear"])).unwrap().unwrap();

        let Request::Cache(request) = command.request else {
            panic!("expected cache request");
        };
        assert_eq!(request.action, CacheAction::Clear);
    }

    #[test]
    fn route_world_cache_scope_requires_both_endpoints() {
        let error = resolve_world_cache_compute_scope(
            Some(WorldCacheComputeScope::Route { routes: Vec::new() }),
            Some("/tmp/a.jpg".to_owned()),
            None,
            Vec::new(),
            Vec::new(),
            None,
            None,
        )
        .unwrap_err();

        assert!(error.contains("requires --to"));
    }

    #[test]
    fn route_world_cache_scope_accepts_repeated_routes() {
        let scope = resolve_world_cache_compute_scope(
            Some(WorldCacheComputeScope::Route { routes: Vec::new() }),
            None,
            None,
            vec![
                WorldCacheRoute {
                    from: "/tmp/a.jpg".to_owned(),
                    to: "/tmp/b.jpg".to_owned(),
                },
                WorldCacheRoute {
                    from: "/tmp/c.jpg".to_owned(),
                    to: "/tmp/d.jpg".to_owned(),
                },
            ],
            Vec::new(),
            None,
            None,
        )
        .unwrap();

        assert_eq!(
            scope,
            WorldCacheComputeScope::Route {
                routes: vec![
                    WorldCacheRoute {
                        from: "/tmp/a.jpg".to_owned(),
                        to: "/tmp/b.jpg".to_owned(),
                    },
                    WorldCacheRoute {
                        from: "/tmp/c.jpg".to_owned(),
                        to: "/tmp/d.jpg".to_owned(),
                    },
                ],
            }
        );
    }

    #[test]
    fn world_cache_compute_options_accept_background() {
        let options = parse_world_cache_compute_options(&args(&[
            "--scope",
            "all",
            "--background",
            "--progress",
        ]))
        .unwrap();

        assert_eq!(options.execution, WorldCacheComputeExecution::Background);
        assert!(options.progress);
        assert_eq!(options.workers, DEFAULT_CANVAS_CACHE_WORKERS);
        assert_eq!(options.scope, WorldCacheComputeScope::All);
    }

    #[test]
    fn world_cache_compute_options_accept_workers() {
        let options =
            parse_world_cache_compute_options(&args(&["--scope", "all", "--workers", "4"]))
                .unwrap();

        assert_eq!(options.workers, 4);
    }

    #[test]
    fn world_cache_compute_options_reject_background_dry_run() {
        let error = parse_world_cache_compute_options(&args(&[
            "--scope",
            "all",
            "--background",
            "--dry-run",
        ]))
        .unwrap_err();

        assert!(error.contains("--background cannot be combined with --dry-run"));
    }

    #[test]
    fn world_cache_compute_options_accept_neighborhood_scope() {
        let path = env::temp_dir().join(format!(
            "muralctl-world-neighborhood-test-{}",
            process::id()
        ));
        fs::write(&path, b"x").unwrap();
        let canonical = path.canonicalize().unwrap().to_string_lossy().into_owned();

        let options = parse_world_cache_compute_options(&args(&[
            "--scope",
            "neighborhood",
            "--center",
            &path.to_string_lossy(),
            "--radius",
            "1",
            "--lod",
            "0",
            "--progress",
        ]))
        .unwrap();

        assert_eq!(
            options.scope,
            WorldCacheComputeScope::Neighborhood {
                centers: vec![canonical],
                radius: 1,
                lod: 0,
            }
        );
        assert!(options.progress);
    }

    #[test]
    fn neighborhood_world_tile_keys_clamp_to_grid_edges() {
        let cells = (0..40 * 40)
            .map(|index| WorldCellCacheEntry {
                source_path: format!("/walls/{index}.jpg"),
                cache_key: format!("cell-key-{index}"),
                image_path: PathBuf::from(format!("/cache/cells/{index}.png")),
            })
            .collect::<Vec<_>>();
        let keys =
            neighborhood_world_tile_keys_for_center(&cells, 40, 40, "/walls/0.jpg", 1, 0).unwrap();

        assert_eq!(
            keys,
            BTreeSet::from([(0, 0, 0), (0, 0, 1), (0, 1, 0), (0, 1, 1)])
        );
    }

    #[test]
    fn neighborhood_world_tile_keys_include_radius_around_center_tile() {
        let cells = (0..40 * 40)
            .map(|index| WorldCellCacheEntry {
                source_path: format!("/walls/{index}.jpg"),
                cache_key: format!("cell-key-{index}"),
                image_path: PathBuf::from(format!("/cache/cells/{index}.png")),
            })
            .collect::<Vec<_>>();
        let center_index = 18 * 40 + 18;
        let keys = neighborhood_world_tile_keys_for_center(
            &cells,
            40,
            40,
            &format!("/walls/{center_index}.jpg"),
            1,
            0,
        )
        .unwrap();

        assert_eq!(keys.len(), 9);
        assert!(keys.contains(&(0, 1, 1)));
        assert!(keys.contains(&(0, 2, 2)));
        assert!(keys.contains(&(0, 3, 3)));
    }

    #[test]
    fn world_lod_plan_counts_preserve_lod_breakdown() {
        let plans = world_lod_plan_counts(&[
            WorldTileCacheEntry {
                lod: 1,
                tile_column: 0,
                tile_row: 0,
                start_column: 0,
                end_column: 64,
                start_row: 0,
                end_row: 64,
                image_path: PathBuf::from("/cache/l1/0-0.png"),
            },
            WorldTileCacheEntry {
                lod: 0,
                tile_column: 0,
                tile_row: 0,
                start_column: 0,
                end_column: 8,
                start_row: 0,
                end_row: 8,
                image_path: PathBuf::from("/cache/l0/0-0.png"),
            },
            WorldTileCacheEntry {
                lod: 1,
                tile_column: 1,
                tile_row: 0,
                start_column: 64,
                end_column: 128,
                start_row: 0,
                end_row: 64,
                image_path: PathBuf::from("/cache/l1/0-1.png"),
            },
        ]);

        assert_eq!(
            plans,
            vec![
                WorldLodPlan {
                    lod: 0,
                    tile_count: 1,
                    tile_ready: 0,
                    tile_missing: 1,
                },
                WorldLodPlan {
                    lod: 1,
                    tile_count: 2,
                    tile_ready: 0,
                    tile_missing: 2,
                },
            ]
        );
    }

    #[test]
    fn selected_world_cell_indices_skip_ready_tile_cells() {
        let root = temp_dir("selected-ready-tile-cells");
        let ready_tile = root.join("ready.png");
        fs::write(&ready_tile, b"ready").unwrap();
        let tiles = vec![
            WorldTileCacheEntry {
                lod: 0,
                tile_column: 0,
                tile_row: 0,
                start_column: 0,
                end_column: 2,
                start_row: 0,
                end_row: 2,
                image_path: ready_tile,
            },
            WorldTileCacheEntry {
                lod: 0,
                tile_column: 1,
                tile_row: 0,
                start_column: 2,
                end_column: 4,
                start_row: 0,
                end_row: 2,
                image_path: root.join("missing.png"),
            },
        ];

        let selected = selected_world_cell_indices(16, 4, None, &tiles);

        assert_eq!(selected, vec![2, 3, 6, 7]);
    }

    #[test]
    fn selected_world_cell_indices_keep_explicit_cell_limit() {
        let root = temp_dir("selected-explicit-limit");
        let ready_tile = root.join("ready.png");
        fs::write(&ready_tile, b"ready").unwrap();
        let tiles = vec![WorldTileCacheEntry {
            lod: 0,
            tile_column: 0,
            tile_row: 0,
            start_column: 0,
            end_column: 2,
            start_row: 0,
            end_row: 2,
            image_path: ready_tile,
        }];

        let selected = selected_world_cell_indices(16, 4, Some(3), &tiles);

        assert_eq!(selected, vec![0, 1, 2]);
    }

    #[test]
    fn selected_world_cell_indices_keep_every_cell_for_full_all_scope() {
        let root = temp_dir("selected-full-all-scope");
        let ready_tile = root.join("ready.png");
        fs::write(&ready_tile, b"ready").unwrap();
        let tiles = vec![WorldTileCacheEntry {
            lod: 0,
            tile_column: 0,
            tile_row: 0,
            start_column: 0,
            end_column: 2,
            start_row: 0,
            end_row: 2,
            image_path: ready_tile,
        }];

        let selected = selected_world_cell_indices_for_scope(
            &WorldCacheComputeScope::All,
            5,
            4,
            None,
            None,
            &tiles,
        );

        assert_eq!(selected, vec![0, 1, 2, 3, 4]);
    }

    #[test]
    fn selected_world_cell_indices_omits_route_scope_persistent_cells() {
        let root = temp_dir("selected-route-scope");
        let tiles = vec![WorldTileCacheEntry {
            lod: 1,
            tile_column: 0,
            tile_row: 0,
            start_column: 0,
            end_column: 4,
            start_row: 0,
            end_row: 4,
            image_path: root.join("missing.png"),
        }];

        let selected = selected_world_cell_indices_for_scope(
            &WorldCacheComputeScope::Route { routes: Vec::new() },
            16,
            4,
            None,
            None,
            &tiles,
        );

        assert!(selected.is_empty());
    }

    #[test]
    fn estimate_missing_cell_bytes_uses_selected_ready_average() {
        let root = temp_dir("estimate-cell-average");
        let ready = root.join("ready.png");
        fs::write(&ready, b"0123456789").unwrap();
        let cells = vec![
            WorldCellCacheEntry {
                source_path: "/wall/ready.jpg".to_owned(),
                cache_key: "ready".to_owned(),
                image_path: ready,
            },
            WorldCellCacheEntry {
                source_path: "/wall/missing-a.jpg".to_owned(),
                cache_key: "missing-a".to_owned(),
                image_path: root.join("missing-a.png"),
            },
            WorldCellCacheEntry {
                source_path: "/wall/missing-b.jpg".to_owned(),
                cache_key: "missing-b".to_owned(),
                image_path: root.join("missing-b.png"),
            },
        ];

        assert_eq!(estimate_missing_cell_bytes(&cells, &[0, 1, 2]), 20);
    }

    #[test]
    fn estimate_missing_tile_bytes_uses_same_lod_ready_average() {
        let root = temp_dir("estimate-tile-average");
        let ready = root.join("ready.png");
        fs::write(&ready, b"0123456789ab").unwrap();
        let tiles = vec![
            WorldTileCacheEntry {
                lod: 1,
                tile_column: 0,
                tile_row: 0,
                start_column: 0,
                end_column: 64,
                start_row: 0,
                end_row: 64,
                image_path: ready,
            },
            WorldTileCacheEntry {
                lod: 1,
                tile_column: 1,
                tile_row: 0,
                start_column: 64,
                end_column: 128,
                start_row: 0,
                end_row: 64,
                image_path: root.join("missing.png"),
            },
        ];

        assert_eq!(estimate_missing_tile_bytes(&tiles), 12);
    }

    #[test]
    fn estimate_world_tile_units_match_lod_generation_shape() {
        let tile = WorldTileCacheEntry {
            lod: 1,
            tile_column: 0,
            tile_row: 0,
            start_column: 0,
            end_column: 64,
            start_row: 0,
            end_row: 64,
            image_path: PathBuf::from("/cache/l1.png"),
        };

        assert_eq!(estimated_world_tile_pixel_units(&tile), (8, 8));
    }

    #[test]
    fn format_rate_per_second_uses_millisecond_precision() {
        assert_eq!(
            format_rate_per_second(150, Duration::from_secs(2)),
            "75.000"
        );
        assert_eq!(format_rate_per_second(1, Duration::from_secs(3)), "0.333");
    }

    #[test]
    fn format_rate_per_second_handles_zero_elapsed() {
        assert_eq!(format_rate_per_second(2, Duration::ZERO), "2000.000");
    }

    fn test_world_cache_status(root: &Path) -> WorldCacheStatus {
        WorldCacheStatus {
            wall_dir: root.join("walls"),
            state_dir: root.join("state"),
            cache_dir: root.join("cache"),
            manifest_path: root.join("cache/manifest"),
            library_count: 0,
            columns: 1,
            rows: 0,
            fingerprint: 0,
            order_policy: mural_core::world_cache::WORLD_ORDER_POLICY.to_owned(),
            thumbnail_edge: DEFAULT_WORLD_CELL_THUMBNAIL_EDGE,
            cell_ready: 0,
            cell_missing: 0,
            world_tile_ready: 0,
            world_tile_missing: 0,
            world_lods: Vec::new(),
            manifest_state: mural_core::world_cache::ManifestState::Missing,
            ready: false,
            message: String::new(),
        }
    }

    #[test]
    fn failure_log_entry_count_ignores_blank_lines() {
        assert_eq!(
            failure_log_entry_count("cell\ta\tbad\n\n\t\n tile\tb\tbad\n"),
            2
        );
    }

    #[test]
    fn world_cache_failure_log_sanitizes_and_counts_entries() {
        let root = temp_dir("failure-log");
        let status = test_world_cache_status(&root);
        let failures = vec![WorldCacheFailure {
            kind: "cell",
            item: "/wall/a\tb.jpg".to_owned(),
            message: "first\nsecond".to_owned(),
        }];

        write_world_cache_failure_log(&status, &failures).unwrap();

        assert_eq!(world_cache_failure_count(&status), 1);
        let content = fs::read_to_string(world_cache_failure_log_path(&status)).unwrap();
        assert_eq!(content, "cell\t/wall/a b.jpg\tfirst second\n");
    }

    #[test]
    fn parse_world_cache_failure_records_reads_tsv() {
        let records = parse_world_cache_failure_records(
            "cell\t/wall/bad.jpg\tdecode failed\n\ntile\tl0 r1 c2\tmissing cell\n",
        );

        assert_eq!(
            records,
            vec![
                WorldCacheFailureRecord {
                    kind: "cell".to_owned(),
                    item: "/wall/bad.jpg".to_owned(),
                    message: "decode failed".to_owned(),
                },
                WorldCacheFailureRecord {
                    kind: "tile".to_owned(),
                    item: "l0 r1 c2".to_owned(),
                    message: "missing cell".to_owned(),
                },
            ]
        );
    }

    #[test]
    fn encode_world_cache_failures_json_escapes_records() {
        let records = vec![WorldCacheFailureRecord {
            kind: "cell".to_owned(),
            item: "/wall/bad\"name.jpg".to_owned(),
            message: "bad\\decode".to_owned(),
        }];

        let encoded = encode_world_cache_failures(&records);

        assert!(encoded.contains("\"kind\":\"cell\""));
        assert!(encoded.contains("/wall/bad\\\"name.jpg"));
        assert!(encoded.contains("bad\\\\decode"));
    }

    #[test]
    fn empty_world_cache_failure_log_clears_count() {
        let root = temp_dir("failure-log-empty");
        let status = test_world_cache_status(&root);
        let failures = vec![WorldCacheFailure {
            kind: "tile",
            item: "l0 r0 c0".to_owned(),
            message: "bad".to_owned(),
        }];
        write_world_cache_failure_log(&status, &failures).unwrap();
        write_world_cache_failure_log(&status, &[]).unwrap();

        assert_eq!(world_cache_failure_count(&status), 0);
        assert_eq!(
            fs::read_to_string(world_cache_failure_log_path(&status)).unwrap(),
            ""
        );
    }

    #[test]
    fn route_world_tile_entries_keep_only_intersecting_tiles() {
        let cells = (0..64)
            .map(|index| WorldCellCacheEntry {
                source_path: format!("/wall/{index:02}.jpg"),
                cache_key: format!("key-{index}"),
                image_path: PathBuf::from(format!("/cache/{index:02}.png")),
            })
            .collect::<Vec<_>>();
        let tiles = vec![
            WorldTileCacheEntry {
                lod: 0,
                tile_column: 0,
                tile_row: 0,
                start_column: 0,
                end_column: 8,
                start_row: 0,
                end_row: 8,
                image_path: PathBuf::from("/cache/tile-0-0.png"),
            },
            WorldTileCacheEntry {
                lod: 0,
                tile_column: 1,
                tile_row: 0,
                start_column: 8,
                end_column: 16,
                start_row: 0,
                end_row: 8,
                image_path: PathBuf::from("/cache/tile-0-1.png"),
            },
        ];
        let lods = vec![WorldLodCacheStatus {
            lod: 0,
            tile_ready: 0,
            tile_missing: 2,
        }];

        let selected = route_world_tile_entries(
            &cells,
            8,
            &lods,
            tiles,
            &[WorldCacheRoute {
                from: "/wall/00.jpg".to_owned(),
                to: "/wall/63.jpg".to_owned(),
            }],
        )
        .unwrap();

        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].tile_row, 0);
        assert_eq!(selected[0].tile_column, 0);
    }

    #[test]
    fn route_world_tile_entries_select_only_rendered_lod() {
        let cells = (0..10_000)
            .map(|index| WorldCellCacheEntry {
                source_path: format!("/wall/{index:05}.jpg"),
                cache_key: format!("key-{index}"),
                image_path: PathBuf::from(format!("/cache/{index:05}.png")),
            })
            .collect::<Vec<_>>();
        let lods = vec![
            WorldLodCacheStatus {
                lod: 0,
                tile_ready: 0,
                tile_missing: 169,
            },
            WorldLodCacheStatus {
                lod: 1,
                tile_ready: 0,
                tile_missing: 4,
            },
        ];
        let tiles = (0..13)
            .flat_map(|row| {
                (0..13).map(move |column| WorldTileCacheEntry {
                    lod: 0,
                    tile_column: column,
                    tile_row: row,
                    start_column: column * 8,
                    end_column: (column + 1) * 8,
                    start_row: row * 8,
                    end_row: (row + 1) * 8,
                    image_path: PathBuf::from(format!("/cache/l0/{row}-{column}.png")),
                })
            })
            .chain((0..2).flat_map(|row| {
                (0..2).map(move |column| WorldTileCacheEntry {
                    lod: 1,
                    tile_column: column,
                    tile_row: row,
                    start_column: column * 64,
                    end_column: (column + 1) * 64,
                    start_row: row * 64,
                    end_row: (row + 1) * 64,
                    image_path: PathBuf::from(format!("/cache/l1/{row}-{column}.png")),
                })
            }))
            .collect::<Vec<_>>();

        let selected = route_world_tile_entries(
            &cells,
            100,
            &lods,
            tiles,
            &[WorldCacheRoute {
                from: "/wall/00000.jpg".to_owned(),
                to: "/wall/09999.jpg".to_owned(),
            }],
        )
        .unwrap();
        let selected_keys = selected
            .iter()
            .map(|entry| (entry.lod, entry.tile_row, entry.tile_column))
            .collect::<BTreeSet<_>>();

        assert_eq!(
            selected_keys,
            BTreeSet::from([(1, 0, 0), (1, 0, 1), (1, 1, 0), (1, 1, 1)])
        );
    }

    #[test]
    fn generate_lod_tile_directly_from_cell_thumbnails() {
        let root = temp_dir("direct-lod");
        let cell_dir = root.join("cells");
        fs::create_dir_all(&cell_dir).unwrap();
        let cells = (0_u8..16)
            .map(|index| {
                let image_path = cell_dir.join(format!("{index}.png"));
                RgbaImage::from_pixel(1, 1, Rgba([index, 0, 0, 255]))
                    .save_with_format(&image_path, ImageFormat::Png)
                    .unwrap();
                WorldCellCacheEntry {
                    source_path: format!("/wall/{index:02}.jpg"),
                    cache_key: format!("key-{index}"),
                    image_path,
                }
            })
            .collect::<Vec<_>>();
        let tile = WorldTileCacheEntry {
            lod: 1,
            tile_column: 0,
            tile_row: 0,
            start_column: 0,
            end_column: 4,
            start_row: 0,
            end_row: 4,
            image_path: root.join("l1.png"),
        };

        assert!(generate_world_tile(&tile, &cells, 4, 1).unwrap().is_empty());

        let image = image::ImageReader::open(&tile.image_path)
            .unwrap()
            .decode()
            .unwrap();
        assert_eq!(image.width(), DEFAULT_WORLD_CELL_THUMBNAIL_EDGE);
        assert_eq!(image.height(), DEFAULT_WORLD_CELL_THUMBNAIL_EDGE);
    }

    #[test]
    fn generate_lod_tile_can_use_source_images_without_cell_cache() {
        let root = temp_dir("direct-lod-sources");
        let source_dir = root.join("sources");
        let cell_dir = root.join("cells");
        fs::create_dir_all(&source_dir).unwrap();
        fs::create_dir_all(&cell_dir).unwrap();
        let cells = (0_u8..16)
            .map(|index| {
                let source_path = source_dir.join(format!("{index}.png"));
                RgbaImage::from_pixel(4, 4, Rgba([index, 0, 0, 255]))
                    .save_with_format(&source_path, ImageFormat::Png)
                    .unwrap();
                WorldCellCacheEntry {
                    source_path: source_path.display().to_string(),
                    cache_key: format!("key-{index}"),
                    image_path: cell_dir.join(format!("{index}.png")),
                }
            })
            .collect::<Vec<_>>();
        let tile = WorldTileCacheEntry {
            lod: 1,
            tile_column: 0,
            tile_row: 0,
            start_column: 0,
            end_column: 4,
            start_row: 0,
            end_row: 4,
            image_path: root.join("l1.png"),
        };

        assert!(generate_world_tile(&tile, &cells, 4, 2).unwrap().is_empty());

        let image = image::ImageReader::open(&tile.image_path)
            .unwrap()
            .decode()
            .unwrap();
        assert_eq!(image.width(), DEFAULT_WORLD_CELL_THUMBNAIL_EDGE);
        assert_eq!(image.height(), DEFAULT_WORLD_CELL_THUMBNAIL_EDGE);
        assert!(!cells[0].image_path.is_file());
    }

    #[test]
    fn generate_lod_tile_records_bad_source_cells_without_failing_tile() {
        let root = temp_dir("direct-lod-bad-source");
        let source_dir = root.join("sources");
        let cell_dir = root.join("cells");
        fs::create_dir_all(&source_dir).unwrap();
        fs::create_dir_all(&cell_dir).unwrap();
        let cells = (0_u8..4)
            .map(|index| {
                let source_path = source_dir.join(format!("{index}.png"));
                if index == 2 {
                    fs::write(&source_path, b"not an image").unwrap();
                } else {
                    RgbaImage::from_pixel(4, 4, Rgba([index, 0, 0, 255]))
                        .save_with_format(&source_path, ImageFormat::Png)
                        .unwrap();
                }
                WorldCellCacheEntry {
                    source_path: source_path.display().to_string(),
                    cache_key: format!("key-{index}"),
                    image_path: cell_dir.join(format!("{index}.png")),
                }
            })
            .collect::<Vec<_>>();
        let tile = WorldTileCacheEntry {
            lod: 1,
            tile_column: 0,
            tile_row: 0,
            start_column: 0,
            end_column: 4,
            start_row: 0,
            end_row: 1,
            image_path: root.join("l1.png"),
        };

        let failures = generate_world_tile(&tile, &cells, 4, 2).unwrap();

        assert!(tile.image_path.is_file());
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].kind, "cell");
        assert_eq!(failures[0].item, cells[2].source_path);
    }
}
