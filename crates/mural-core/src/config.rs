use std::collections::BTreeMap;
use std::env;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use mural_ipc::{
    CacheBackend, CanvasMode, CanvasPanAxis, CanvasTileCount, CanvasWalk, DEFAULT_CANVAS_IN_MS,
    DEFAULT_CANVAS_OUT_MS, DEFAULT_CANVAS_OVERVIEW_SCALE, DEFAULT_CANVAS_PAN_MS,
    DEFAULT_CANVAS_THUMBNAIL_MAX_EDGE, DEFAULT_DURATION_MS, Easing, MAX_CANVAS_TILE_COUNT,
    PushDirection, PushMode, ScaleMode, Transition, TransitionKind, WallpaperAction,
    transition_descriptor, transition_registry, validate_canvas_mode_walk,
};

use crate::actions::{ActionMap, ActionTransitions};

const DEFAULT_CANVAS_CACHE_MEMORY_MIB: usize = 512;
const MIB_BYTES: usize = 1024 * 1024;

#[derive(Clone, Debug)]
pub struct MuralConfig {
    pub wall_dir: Option<PathBuf>,
    pub state_dir: Option<PathBuf>,
    pub quarantine_dir: Option<PathBuf>,
    pub favorite_weight: Option<usize>,
    pub max_history: Option<usize>,
    pub scale_mode: ScaleMode,
    pub decode_full_workers: usize,
    pub canvas_thumbnail_max_edge: u32,
    pub canvas_cache_workers: usize,
    pub canvas_cache_backend: CacheBackend,
    pub canvas_cache_memory_bytes: usize,
    pub(crate) actions: ActionMap,
    pub(crate) world_fallback_transition: Option<Transition>,
}

impl MuralConfig {
    pub fn load() -> Result<Self, String> {
        let mut raw = RawConfig::default();
        let Some(path) = config_path() else {
            raw.apply_env()?;
            return raw.build();
        };

        if !path.exists() {
            if env::var_os("MURAL_CONFIG").is_some() {
                return Err(format!("MURAL_CONFIG does not exist: {}", path.display()));
            }
            raw.apply_env()?;
            return raw.build();
        }

        raw.apply_file(&path)?;
        raw.apply_env()?;
        raw.build()
    }

    #[must_use]
    pub fn transition_for_action(&self, action: &WallpaperAction) -> Transition {
        self.actions.transition_for_wallpaper_action(action)
    }

    #[must_use]
    pub fn startup_transition(&self) -> Transition {
        self.actions.startup_transition()
    }

    #[must_use]
    pub fn uses_canvas_transition(&self) -> bool {
        self.actions
            .transitions()
            .iter()
            .any(|transition| matches!(transition, Transition::Canvas { .. }))
    }

    #[must_use]
    pub fn uses_world_transition(&self) -> bool {
        self.actions
            .transitions()
            .iter()
            .any(|transition| matches!(transition, Transition::World { .. }))
    }

    #[must_use]
    pub fn canvas_prewarm_transition(&self) -> Option<Transition> {
        self.actions
            .transitions()
            .iter()
            .filter(|transition| matches!(transition, Transition::Canvas { .. }))
            .max_by_key(|transition| match transition {
                Transition::Canvas { tile_count, .. } => canvas_tile_count_sort_key(*tile_count),
                Transition::Cut
                | Transition::Fade { .. }
                | Transition::World { .. }
                | Transition::Push { .. } => 0,
            })
            .copied()
    }

    #[must_use]
    pub const fn world_fallback_transition(&self) -> Option<Transition> {
        self.world_fallback_transition
    }
}

fn canvas_tile_count_sort_key(tile_count: CanvasTileCount) -> usize {
    match tile_count {
        CanvasTileCount::Fixed(count) | CanvasTileCount::Auto { max: Some(count) } => count,
        CanvasTileCount::Auto { max: None } => MAX_CANVAS_TILE_COUNT,
    }
}

#[derive(Clone, Debug)]
struct RawConfig {
    wall_dir: Option<PathBuf>,
    state_dir: Option<PathBuf>,
    quarantine_dir: Option<PathBuf>,
    favorite_weight: Option<usize>,
    max_history: Option<usize>,
    scale_mode: ScaleMode,
    duration_ms: u64,
    easing: Easing,
    push_mode: PushMode,
    canvas_out_ms: u64,
    canvas_pan_ms: u64,
    canvas_in_ms: u64,
    canvas_mode: CanvasMode,
    canvas_walk: CanvasWalk,
    canvas_pan_axis: CanvasPanAxis,
    canvas_overview_scale: f32,
    canvas_tile_count: RawCanvasTileCount,
    canvas_max_tile_count: Option<usize>,
    canvas_thumbnail_max_edge: u32,
    canvas_cache_workers: usize,
    canvas_cache_backend: CacheBackend,
    canvas_cache_memory_mib: usize,
    decode_full_workers: usize,
    transition_profiles: BTreeMap<String, RawTransitionProfile>,
    transition_next: String,
    transition_back: String,
    transition_shift_forward: String,
    transition_shift_back: String,
    transition_replace: String,
    transition_quarantine: String,
    transition_startup: String,
}

impl Default for RawConfig {
    fn default() -> Self {
        Self {
            wall_dir: None,
            state_dir: None,
            quarantine_dir: None,
            favorite_weight: None,
            max_history: None,
            scale_mode: ScaleMode::Fill,
            duration_ms: DEFAULT_DURATION_MS,
            easing: Easing::EaseOutCubic,
            push_mode: PushMode::Portal,
            canvas_out_ms: DEFAULT_CANVAS_OUT_MS,
            canvas_pan_ms: DEFAULT_CANVAS_PAN_MS,
            canvas_in_ms: DEFAULT_CANVAS_IN_MS,
            canvas_mode: CanvasMode::Clipped,
            canvas_walk: CanvasWalk::Paged,
            canvas_pan_axis: CanvasPanAxis::Auto,
            canvas_overview_scale: DEFAULT_CANVAS_OVERVIEW_SCALE,
            canvas_tile_count: RawCanvasTileCount::Auto,
            canvas_max_tile_count: None,
            canvas_thumbnail_max_edge: DEFAULT_CANVAS_THUMBNAIL_MAX_EDGE,
            canvas_cache_workers: 1,
            canvas_cache_backend: CacheBackend::Auto,
            canvas_cache_memory_mib: DEFAULT_CANVAS_CACHE_MEMORY_MIB,
            decode_full_workers: 2,
            transition_profiles: BTreeMap::new(),
            transition_next: "push:up".to_owned(),
            transition_back: "push:down".to_owned(),
            transition_shift_forward: "push:left".to_owned(),
            transition_shift_back: "push:right".to_owned(),
            transition_replace: "cut".to_owned(),
            transition_quarantine: "cut".to_owned(),
            transition_startup: "cut".to_owned(),
        }
    }
}

#[derive(Clone, Debug, Default)]
struct RawTransitionProfile {
    kind: Option<TransitionKind>,
    direction: Option<PushDirection>,
    duration_ms: Option<u64>,
    easing: Option<Easing>,
    mode_value: Option<String>,
    push_mode: Option<PushMode>,
    canvas_mode: Option<CanvasMode>,
    canvas_walk: Option<CanvasWalk>,
    canvas_out_ms: Option<u64>,
    canvas_pan_ms: Option<u64>,
    canvas_in_ms: Option<u64>,
    canvas_pan_axis: Option<CanvasPanAxis>,
    canvas_overview_scale: Option<f32>,
    canvas_tile_count: Option<RawCanvasTileCount>,
    canvas_max_tile_count: Option<usize>,
    fallback: Option<String>,
}

impl RawTransitionProfile {
    fn apply_mode(
        &mut self,
        profile_name: &str,
        path: &Path,
        line_number: usize,
        value: &str,
    ) -> Result<(), String> {
        value.clone_into(self.mode_value.get_or_insert_default());
        match profile_name {
            "push" => {
                self.push_mode = Some(
                    PushMode::parse(value)
                        .map_err(|error| config_error(path, line_number, error))?,
                );
                self.canvas_mode = None;
            }
            "canvas" => {
                self.push_mode = None;
                self.canvas_mode = Some(
                    CanvasMode::parse(value)
                        .map_err(|error| config_error(path, line_number, error))?,
                );
            }
            _ => {
                let (push_mode, canvas_mode) = parse_transition_mode(value)
                    .map_err(|error| config_error(path, line_number, error))?;
                self.push_mode = push_mode;
                self.canvas_mode = canvas_mode;
            }
        }
        Ok(())
    }

    fn populated_fields(&self) -> [(&'static str, bool); 13] {
        [
            ("direction", self.direction.is_some()),
            ("duration_ms", self.duration_ms.is_some()),
            ("easing", self.easing.is_some()),
            ("mode", self.mode_value.is_some()),
            ("walk", self.canvas_walk.is_some()),
            ("zoom_out_ms", self.canvas_out_ms.is_some()),
            ("pan_ms", self.canvas_pan_ms.is_some()),
            ("zoom_in_ms", self.canvas_in_ms.is_some()),
            ("pan_axis", self.canvas_pan_axis.is_some()),
            ("overview_scale", self.canvas_overview_scale.is_some()),
            ("tile_count", self.canvas_tile_count.is_some()),
            ("max_tile_count", self.canvas_max_tile_count.is_some()),
            ("fallback", self.fallback.is_some()),
        ]
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RawCanvasTileCount {
    Auto,
    Fixed(usize),
}

impl RawConfig {
    fn apply_file(&mut self, path: &Path) -> Result<(), String> {
        let content = fs::read_to_string(path)
            .map_err(|error| format!("failed to read config {}: {error}", path.display()))?;

        for (line_index, raw_line) in content.lines().enumerate() {
            let line_number = line_index + 1;
            let line = strip_inline_comment(raw_line).trim();
            if line.is_empty() {
                continue;
            }
            let (raw_key, raw_value) = line
                .split_once('=')
                .ok_or_else(|| format!("{}:{line_number}: expected key = value", path.display()))?;
            let key = raw_key.trim();
            let value = unquote(raw_value.trim());
            self.apply_value(path, line_number, key, value)?;
        }

        Ok(())
    }

    fn apply_env(&mut self) -> Result<(), String> {
        if let Ok(value) = env::var("MURAL_SCALE_MODE") {
            self.scale_mode =
                ScaleMode::parse(&value).map_err(|error| format!("MURAL_SCALE_MODE: {error}"))?;
        }
        if let Ok(value) = env::var("MURAL_CANVAS_ZOOM_OUT_MS") {
            self.canvas_out_ms = parse_positive_u64_env("MURAL_CANVAS_ZOOM_OUT_MS", &value)?;
        }
        if let Ok(value) = env::var("MURAL_CANVAS_PAN_MS") {
            self.canvas_pan_ms = parse_positive_u64_env("MURAL_CANVAS_PAN_MS", &value)?;
        }
        if let Ok(value) = env::var("MURAL_CANVAS_ZOOM_IN_MS") {
            self.canvas_in_ms = parse_positive_u64_env("MURAL_CANVAS_ZOOM_IN_MS", &value)?;
        }
        if let Ok(value) = env::var("MURAL_CANVAS_MODE") {
            self.canvas_mode =
                CanvasMode::parse(&value).map_err(|error| format!("MURAL_CANVAS_MODE: {error}"))?;
        }
        if let Ok(value) = env::var("MURAL_CANVAS_WALK") {
            self.canvas_walk =
                CanvasWalk::parse(&value).map_err(|error| format!("MURAL_CANVAS_WALK: {error}"))?;
        }
        if let Ok(value) = env::var("MURAL_CANVAS_PAN_AXIS") {
            self.canvas_pan_axis = parse_canvas_pan_axis_env("MURAL_CANVAS_PAN_AXIS", &value)?;
        }
        if let Ok(value) = env::var("MURAL_CANVAS_OVERVIEW_SCALE") {
            self.canvas_overview_scale =
                parse_canvas_overview_scale_env("MURAL_CANVAS_OVERVIEW_SCALE", &value)?;
        }
        if let Ok(value) = env::var("MURAL_CANVAS_TILE_COUNT") {
            self.canvas_tile_count =
                parse_canvas_tile_count_env("MURAL_CANVAS_TILE_COUNT", &value)?;
        }
        if let Ok(value) = env::var("MURAL_CANVAS_MAX_TILE_COUNT") {
            self.canvas_max_tile_count = Some(parse_canvas_max_tile_count_env(
                "MURAL_CANVAS_MAX_TILE_COUNT",
                &value,
            )?);
        }
        if let Ok(value) = env::var("MURAL_CANVAS_THUMBNAIL_MAX_EDGE") {
            self.canvas_thumbnail_max_edge =
                parse_positive_u32_env("MURAL_CANVAS_THUMBNAIL_MAX_EDGE", &value)?;
        }
        if let Ok(value) = env::var("MURAL_CANVAS_CACHE_WORKERS") {
            self.canvas_cache_workers =
                parse_worker_count_env("MURAL_CANVAS_CACHE_WORKERS", &value)?;
        }
        if let Ok(value) = env::var("MURAL_CANVAS_CACHE_BACKEND") {
            self.canvas_cache_backend = CacheBackend::parse(&value)
                .map_err(|error| format!("MURAL_CANVAS_CACHE_BACKEND: {error}"))?;
        }
        if let Ok(value) = env::var("MURAL_CANVAS_CACHE_MEMORY_MIB") {
            self.canvas_cache_memory_mib =
                parse_positive_usize_env("MURAL_CANVAS_CACHE_MEMORY_MIB", &value)?;
        }
        if let Ok(value) = env::var("MURAL_DECODE_FULL_WORKERS") {
            self.decode_full_workers = parse_worker_count_env("MURAL_DECODE_FULL_WORKERS", &value)?;
        }
        Ok(())
    }

    fn apply_value(
        &mut self,
        path: &Path,
        line_number: usize,
        key: &str,
        value: &str,
    ) -> Result<(), String> {
        match key {
            "wall_dir" => self.wall_dir = Some(expand_home(value)),
            "state_dir" => self.state_dir = Some(expand_home(value)),
            "quarantine_dir" => self.quarantine_dir = Some(expand_home(value)),
            "favorite_weight" => {
                self.favorite_weight = Some(parse_positive_usize(path, line_number, key, value)?);
            }
            "max_history" => {
                self.max_history = Some(parse_positive_usize(path, line_number, key, value)?);
            }
            "scale_mode" => {
                self.scale_mode = ScaleMode::parse(value)
                    .map_err(|error| config_error(path, line_number, error))?;
            }
            "canvas.thumbnail_max_edge" => {
                self.canvas_thumbnail_max_edge = parse_positive_u32(path, line_number, key, value)?;
            }
            "canvas.cache.workers" => {
                self.canvas_cache_workers = parse_worker_count(path, line_number, key, value)?;
            }
            "canvas.cache.backend" => {
                self.canvas_cache_backend = CacheBackend::parse(value)
                    .map_err(|error| config_error(path, line_number, error))?;
            }
            "canvas.cache.memory_mib" => {
                self.canvas_cache_memory_mib = parse_positive_usize(path, line_number, key, value)?;
            }
            "decode.full_workers" => {
                self.decode_full_workers = parse_worker_count(path, line_number, key, value)?;
            }
            "action.next" => value.clone_into(&mut self.transition_next),
            "action.back" => value.clone_into(&mut self.transition_back),
            "action.shift_forward" => value.clone_into(&mut self.transition_shift_forward),
            "action.shift_back" => value.clone_into(&mut self.transition_shift_back),
            "action.replace" => value.clone_into(&mut self.transition_replace),
            "action.quarantine" => value.clone_into(&mut self.transition_quarantine),
            "action.startup" => value.clone_into(&mut self.transition_startup),
            _ => {
                if self.apply_transition_profile_value(path, line_number, key, value)? {
                    return Ok(());
                }
                return Err(format!(
                    "{}:{line_number}: unknown config key: {key}",
                    path.display()
                ));
            }
        }
        Ok(())
    }

    fn apply_transition_profile_value(
        &mut self,
        path: &Path,
        line_number: usize,
        key: &str,
        value: &str,
    ) -> Result<bool, String> {
        let Some(rest) = key.strip_prefix("transition.") else {
            return Ok(false);
        };
        let Some((profile_name, field)) = rest.split_once('.') else {
            return Ok(false);
        };
        if profile_name.is_empty() || field.is_empty() {
            return Ok(false);
        }

        let profile = self
            .transition_profiles
            .entry(profile_name.to_owned())
            .or_default();
        match field {
            "type" => {
                profile.kind = Some(
                    parse_transition_kind(value)
                        .map_err(|error| config_error(path, line_number, error))?,
                );
            }
            "direction" => {
                profile.direction = Some(
                    parse_push_direction(value)
                        .map_err(|error| config_error(path, line_number, error))?,
                );
            }
            "duration_ms" => {
                profile.duration_ms = Some(parse_positive_u64(path, line_number, key, value)?);
            }
            "easing" => {
                profile.easing = Some(
                    Easing::parse(value).map_err(|error| config_error(path, line_number, error))?,
                );
            }
            "mode" => profile.apply_mode(profile_name, path, line_number, value)?,
            "walk" => {
                profile.canvas_walk = Some(
                    CanvasWalk::parse(value)
                        .map_err(|error| config_error(path, line_number, error))?,
                );
            }
            "zoom_out_ms" => {
                profile.canvas_out_ms = Some(parse_positive_u64(path, line_number, key, value)?);
            }
            "pan_ms" => {
                profile.canvas_pan_ms = Some(parse_positive_u64(path, line_number, key, value)?);
            }
            "zoom_in_ms" => {
                profile.canvas_in_ms = Some(parse_positive_u64(path, line_number, key, value)?);
            }
            "pan_axis" => {
                profile.canvas_pan_axis = Some(
                    parse_canvas_pan_axis(value)
                        .map_err(|error| config_error(path, line_number, error))?,
                );
            }
            "overview_scale" => {
                profile.canvas_overview_scale =
                    Some(parse_canvas_overview_scale(path, line_number, key, value)?);
            }
            "tile_count" => {
                profile.canvas_tile_count =
                    Some(parse_canvas_tile_count(path, line_number, key, value)?);
            }
            "max_tile_count" => {
                profile.canvas_max_tile_count =
                    Some(parse_canvas_max_tile_count(path, line_number, key, value)?);
            }
            "fallback" => {
                if profile_name != "world" {
                    return Err(config_error(
                        path,
                        line_number,
                        format!("transition.{profile_name}.fallback is only supported for world"),
                    ));
                }
                profile.fallback = Some(value.to_owned());
            }
            _ => return Ok(false),
        }

        Ok(true)
    }

    fn build(self) -> Result<MuralConfig, String> {
        self.validate_transition_profiles()?;
        let world_fallback_transition = self.resolve_world_fallback_transition()?;
        let actions = ActionMap::new(&ActionTransitions {
            next: self.resolve_transition_ref(&self.transition_next)?,
            back: self.resolve_transition_ref(&self.transition_back)?,
            shift_forward: self.resolve_transition_ref(&self.transition_shift_forward)?,
            shift_back: self.resolve_transition_ref(&self.transition_shift_back)?,
            replace: self.resolve_transition_ref(&self.transition_replace)?,
            quarantine: self.resolve_transition_ref(&self.transition_quarantine)?,
            startup: self.resolve_transition_ref(&self.transition_startup)?,
        });
        Ok(MuralConfig {
            wall_dir: self.wall_dir,
            state_dir: self.state_dir,
            quarantine_dir: self.quarantine_dir,
            favorite_weight: self.favorite_weight,
            max_history: self.max_history,
            scale_mode: self.scale_mode,
            decode_full_workers: clamp_worker_count(self.decode_full_workers),
            canvas_thumbnail_max_edge: self.canvas_thumbnail_max_edge,
            canvas_cache_workers: clamp_worker_count(self.canvas_cache_workers),
            canvas_cache_backend: self.canvas_cache_backend,
            canvas_cache_memory_bytes: mib_to_bytes(self.canvas_cache_memory_mib),
            actions,
            world_fallback_transition,
        })
    }

    fn validate_transition_profiles(&self) -> Result<(), String> {
        for (name, profile) in &self.transition_profiles {
            let kind = profile
                .kind
                .or_else(|| builtin_transition_kind(name))
                .ok_or_else(|| {
                    format!(
                        "transition profile '{name}' must declare a compiled-in type with transition.{name}.type"
                    )
                })?;
            if let Some(built_in_kind) = builtin_transition_kind(name)
                && profile
                    .kind
                    .is_some_and(|explicit| explicit != built_in_kind)
            {
                return Err(format!(
                    "built-in transition profile '{name}' cannot be retyped"
                ));
            }
            let descriptor = transition_registry()
                .iter()
                .find(|descriptor| descriptor.kind == kind)
                .expect("every transition kind has a registry descriptor");

            for (field, populated) in profile.populated_fields() {
                if !populated {
                    continue;
                }
                let supported = descriptor
                    .parameters
                    .iter()
                    .any(|parameter| parameter.name == field)
                    || (kind == TransitionKind::World && field == "fallback");
                if !supported {
                    return Err(format!(
                        "transition profile '{name}' field '{field}' is not valid for {}",
                        descriptor.name
                    ));
                }
            }

            match kind {
                TransitionKind::Push => {
                    self.resolve_push_mode(name, Some(profile))?;
                }
                TransitionKind::Canvas => {
                    let built_in = self.transition_profiles.get("canvas");
                    let mode = self.resolve_canvas_mode(name, Some(profile))?;
                    let walk = self.resolve_canvas_walk(Some(profile), built_in);
                    validate_canvas_mode_walk(mode, walk).map_err(|error| {
                        format!("transition profile '{name}' is invalid: {error}")
                    })?;
                }
                TransitionKind::World if name != "world" && profile.fallback.is_some() => {
                    return Err(format!(
                        "transition profile '{name}' field 'fallback' is only valid on the built-in world profile"
                    ));
                }
                TransitionKind::Cut | TransitionKind::Fade | TransitionKind::World => {}
            }
        }
        Ok(())
    }

    fn resolve_world_fallback_transition(&self) -> Result<Option<Transition>, String> {
        let Some(reference) = self
            .transition_profiles
            .get("world")
            .and_then(|profile| profile.fallback.as_deref())
        else {
            return Ok(None);
        };
        let transition = self.resolve_transition_ref(reference)?;
        if !matches!(transition, Transition::Cut | Transition::Push { .. }) {
            return Err("transition.world.fallback must reference cut or push".to_owned());
        }
        Ok(Some(transition))
    }

    fn resolve_transition_ref(&self, reference: &str) -> Result<Transition, String> {
        let (name, suffix) = split_transition_ref(reference)?;
        let profile = self.transition_profiles.get(name);
        let kind = profile
            .and_then(|profile| profile.kind)
            .or_else(|| builtin_transition_kind(name))
            .ok_or_else(|| format!("transition profile '{name}' is not defined"))?;

        match kind {
            TransitionKind::Cut => {
                reject_transition_suffix(name, suffix)?;
                Ok(Transition::Cut)
            }
            TransitionKind::Fade => {
                reject_transition_suffix(name, suffix)?;
                let built_in = self.transition_profiles.get("fade");
                Ok(Transition::Fade {
                    duration_ms: profile
                        .and_then(|profile| profile.duration_ms)
                        .or_else(|| built_in.and_then(|profile| profile.duration_ms))
                        .unwrap_or(self.duration_ms),
                    easing: profile
                        .and_then(|profile| profile.easing)
                        .or_else(|| built_in.and_then(|profile| profile.easing))
                        .unwrap_or(self.easing),
                })
            }
            TransitionKind::World => {
                reject_transition_suffix(name, suffix)?;
                let built_in = self.transition_profiles.get("world");
                Ok(Transition::World {
                    duration_ms: profile
                        .and_then(|profile| profile.duration_ms)
                        .or_else(|| built_in.and_then(|profile| profile.duration_ms))
                        .unwrap_or(self.duration_ms),
                    easing: profile
                        .and_then(|profile| profile.easing)
                        .or_else(|| built_in.and_then(|profile| profile.easing))
                        .unwrap_or(self.easing),
                })
            }
            TransitionKind::Push => {
                let built_in = self.transition_profiles.get("push");
                let direction = suffix
                    .map(parse_push_direction)
                    .transpose()?
                    .or_else(|| profile.and_then(|profile| profile.direction))
                    .ok_or_else(|| format!("push transition '{name}' requires a direction"))?;
                Ok(Transition::Push {
                    direction,
                    duration_ms: profile
                        .and_then(|profile| profile.duration_ms)
                        .or_else(|| built_in.and_then(|profile| profile.duration_ms))
                        .unwrap_or(self.duration_ms),
                    easing: profile
                        .and_then(|profile| profile.easing)
                        .or_else(|| built_in.and_then(|profile| profile.easing))
                        .unwrap_or(self.easing),
                    mode: self.resolve_push_mode(name, profile)?,
                })
            }
            TransitionKind::Canvas => {
                let built_in = self.transition_profiles.get("canvas");
                let pan_axis = suffix
                    .map(parse_canvas_pan_axis)
                    .transpose()?
                    .or_else(|| profile.and_then(|profile| profile.canvas_pan_axis))
                    .or_else(|| built_in.and_then(|profile| profile.canvas_pan_axis))
                    .unwrap_or(self.canvas_pan_axis);
                let mode = self.resolve_canvas_mode(name, profile)?;
                let walk = self.resolve_canvas_walk(profile, built_in);
                validate_canvas_mode_walk(mode, walk).map_err(|error| error.to_string())?;
                Ok(Transition::Canvas {
                    zoom_out_ms: profile
                        .and_then(|profile| profile.canvas_out_ms)
                        .or_else(|| built_in.and_then(|profile| profile.canvas_out_ms))
                        .unwrap_or(self.canvas_out_ms),
                    pan_ms: profile
                        .and_then(|profile| profile.canvas_pan_ms)
                        .or_else(|| built_in.and_then(|profile| profile.canvas_pan_ms))
                        .unwrap_or(self.canvas_pan_ms),
                    zoom_in_ms: profile
                        .and_then(|profile| profile.canvas_in_ms)
                        .or_else(|| built_in.and_then(|profile| profile.canvas_in_ms))
                        .unwrap_or(self.canvas_in_ms),
                    easing: profile
                        .and_then(|profile| profile.easing)
                        .or_else(|| built_in.and_then(|profile| profile.easing))
                        .unwrap_or(self.easing),
                    mode,
                    walk,
                    pan_axis,
                    overview_scale: profile
                        .and_then(|profile| profile.canvas_overview_scale)
                        .or_else(|| built_in.and_then(|profile| profile.canvas_overview_scale))
                        .unwrap_or(self.canvas_overview_scale),
                    tile_count: self.resolve_canvas_tile_count(profile, built_in),
                })
            }
        }
    }

    fn resolve_push_mode(
        &self,
        name: &str,
        profile: Option<&RawTransitionProfile>,
    ) -> Result<PushMode, String> {
        if let Some(profile) = profile {
            if let Some(mode) = profile.push_mode {
                return Ok(mode);
            }
            reject_incompatible_transition_mode(name, profile, "push", "portal, screen, or pan")?;
        }

        if name != "push"
            && let Some(built_in) = self.transition_profiles.get("push")
        {
            if let Some(mode) = built_in.push_mode {
                return Ok(mode);
            }
            reject_incompatible_transition_mode(
                "push",
                built_in,
                "push",
                "portal, screen, or pan",
            )?;
        }

        Ok(self.push_mode)
    }

    fn resolve_canvas_mode(
        &self,
        name: &str,
        profile: Option<&RawTransitionProfile>,
    ) -> Result<CanvasMode, String> {
        if let Some(profile) = profile {
            if let Some(mode) = profile.canvas_mode {
                return Ok(mode);
            }
            reject_incompatible_transition_mode(
                name,
                profile,
                "canvas",
                "clipped, morph, overlap, collage, or span",
            )?;
        }

        if name != "canvas"
            && let Some(built_in) = self.transition_profiles.get("canvas")
        {
            if let Some(mode) = built_in.canvas_mode {
                return Ok(mode);
            }
            reject_incompatible_transition_mode(
                "canvas",
                built_in,
                "canvas",
                "clipped, morph, overlap, collage, or span",
            )?;
        }

        Ok(self.canvas_mode)
    }

    fn resolve_canvas_walk(
        &self,
        profile: Option<&RawTransitionProfile>,
        built_in: Option<&RawTransitionProfile>,
    ) -> CanvasWalk {
        profile
            .and_then(|profile| profile.canvas_walk)
            .or_else(|| built_in.and_then(|profile| profile.canvas_walk))
            .unwrap_or(self.canvas_walk)
    }

    fn resolve_canvas_tile_count(
        &self,
        profile: Option<&RawTransitionProfile>,
        built_in: Option<&RawTransitionProfile>,
    ) -> CanvasTileCount {
        let tile_count = profile
            .and_then(|profile| profile.canvas_tile_count)
            .or_else(|| built_in.and_then(|profile| profile.canvas_tile_count))
            .unwrap_or(self.canvas_tile_count);
        let max = profile
            .and_then(|profile| profile.canvas_max_tile_count)
            .or_else(|| built_in.and_then(|profile| profile.canvas_max_tile_count))
            .or(self.canvas_max_tile_count);

        match tile_count {
            RawCanvasTileCount::Auto => CanvasTileCount::Auto { max },
            RawCanvasTileCount::Fixed(count) => CanvasTileCount::Fixed(count),
        }
    }
}

fn config_path() -> Option<PathBuf> {
    config_path_from(
        env::var_os("MURAL_CONFIG"),
        env::var_os("XDG_CONFIG_HOME"),
        env::var_os("HOME"),
    )
}

fn config_path_from(
    mural_config: Option<OsString>,
    xdg_config_home: Option<OsString>,
    home: Option<OsString>,
) -> Option<PathBuf> {
    if let Some(path) = mural_config {
        return Some(expand_home_os(path));
    }
    if let Some(config_home) = absolute_xdg_home(xdg_config_home) {
        return Some(config_home.join("mural/config"));
    }
    home.map(|home| PathBuf::from(home).join(".config/mural/config"))
}

fn absolute_xdg_home(value: Option<OsString>) -> Option<PathBuf> {
    value
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
}

fn expand_home_os(path: impl Into<std::ffi::OsString>) -> PathBuf {
    expand_home(&path.into().to_string_lossy())
}

fn expand_home(path: &str) -> PathBuf {
    if path == "~" {
        return home_dir().unwrap_or_else(|| PathBuf::from(path));
    }
    if let Some(rest) = path.strip_prefix("~/") {
        return home_dir().map_or_else(|| PathBuf::from(path), |home| home.join(rest));
    }
    PathBuf::from(path)
}

fn home_dir() -> Option<PathBuf> {
    env::var_os("HOME").map(PathBuf::from)
}

fn strip_inline_comment(line: &str) -> &str {
    let mut in_single_quote = false;
    let mut in_double_quote = false;
    for (index, character) in line.char_indices() {
        match character {
            '\'' if !in_double_quote => in_single_quote = !in_single_quote,
            '"' if !in_single_quote => in_double_quote = !in_double_quote,
            '#' | ';' if !in_single_quote && !in_double_quote => return &line[..index],
            _ => {}
        }
    }
    line
}

fn unquote(value: &str) -> &str {
    if value.len() >= 2
        && ((value.starts_with('"') && value.ends_with('"'))
            || (value.starts_with('\'') && value.ends_with('\'')))
    {
        &value[1..value.len() - 1]
    } else {
        value
    }
}

fn split_transition_ref(reference: &str) -> Result<(&str, Option<&str>), String> {
    let Some((name, suffix)) = reference.split_once(':') else {
        return Ok((reference, None));
    };
    if name.is_empty() {
        return Err(format!(
            "transition reference '{reference}' has an empty profile name"
        ));
    }
    if suffix.is_empty() {
        return Err(format!(
            "transition reference '{reference}' has an empty override"
        ));
    }
    Ok((name, Some(suffix)))
}

fn parse_push_direction(value: &str) -> Result<PushDirection, String> {
    match value {
        "up" => Ok(PushDirection::Up),
        "down" => Ok(PushDirection::Down),
        "left" => Ok(PushDirection::Left),
        "right" => Ok(PushDirection::Right),
        _ => Err(format!("unknown push direction: {value}")),
    }
}

fn parse_transition_mode(value: &str) -> Result<(Option<PushMode>, Option<CanvasMode>), String> {
    let push_mode = PushMode::parse(value).ok();
    let canvas_mode = CanvasMode::parse(value).ok();
    if push_mode.is_none() && canvas_mode.is_none() {
        return Err(format!("unknown transition mode: {value}"));
    }
    Ok((push_mode, canvas_mode))
}

fn reject_incompatible_transition_mode(
    name: &str,
    profile: &RawTransitionProfile,
    kind: &str,
    expected: &str,
) -> Result<(), String> {
    if let Some(value) = profile.mode_value.as_deref() {
        return Err(format!(
            "transition profile '{name}' mode '{value}' is not valid for {kind}; expected {expected}"
        ));
    }
    Ok(())
}

fn parse_canvas_pan_axis(value: &str) -> Result<CanvasPanAxis, String> {
    CanvasPanAxis::parse(value).map_err(|error| error.to_string())
}

fn parse_transition_kind(value: &str) -> Result<TransitionKind, String> {
    builtin_transition_kind(value)
        .ok_or_else(|| format!("unknown transition profile type: {value}"))
}

fn builtin_transition_kind(name: &str) -> Option<TransitionKind> {
    transition_descriptor(name).map(|descriptor| descriptor.kind)
}

fn reject_transition_suffix(name: &str, suffix: Option<&str>) -> Result<(), String> {
    if suffix.is_some() {
        Err(format!("transition '{name}' does not accept ':override'"))
    } else {
        Ok(())
    }
}

fn parse_positive_usize(
    path: &Path,
    line_number: usize,
    key: &str,
    value: &str,
) -> Result<usize, String> {
    let parsed = value.parse::<usize>().map_err(|error| {
        format!(
            "{}:{line_number}: invalid integer for {key}: {error}",
            path.display()
        )
    })?;
    if parsed == 0 {
        return Err(format!(
            "{}:{line_number}: {key} must be greater than zero",
            path.display()
        ));
    }
    Ok(parsed)
}

fn parse_canvas_tile_count(
    path: &Path,
    line_number: usize,
    key: &str,
    value: &str,
) -> Result<RawCanvasTileCount, String> {
    if value == "auto" {
        return Ok(RawCanvasTileCount::Auto);
    }
    let parsed = parse_positive_usize(path, line_number, key, value)?;
    if parsed > MAX_CANVAS_TILE_COUNT {
        return Err(format!(
            "{}:{line_number}: {key} must be at most {MAX_CANVAS_TILE_COUNT}",
            path.display()
        ));
    }
    Ok(RawCanvasTileCount::Fixed(parsed))
}

fn parse_canvas_max_tile_count(
    path: &Path,
    line_number: usize,
    key: &str,
    value: &str,
) -> Result<usize, String> {
    let parsed = parse_positive_usize(path, line_number, key, value)?;
    if parsed > MAX_CANVAS_TILE_COUNT {
        return Err(format!(
            "{}:{line_number}: {key} must be at most {MAX_CANVAS_TILE_COUNT}",
            path.display()
        ));
    }
    Ok(parsed)
}

fn parse_canvas_overview_scale(
    path: &Path,
    line_number: usize,
    key: &str,
    value: &str,
) -> Result<f32, String> {
    let parsed = value.parse::<f32>().map_err(|error| {
        format!(
            "{}:{line_number}: invalid number for {key}: {error}",
            path.display()
        )
    })?;
    if !parsed.is_finite() || parsed <= 0.0 || parsed > 1.0 {
        return Err(format!(
            "{}:{line_number}: {key} must be greater than 0 and at most 1",
            path.display()
        ));
    }
    Ok(parsed)
}

fn parse_positive_u64(
    path: &Path,
    line_number: usize,
    key: &str,
    value: &str,
) -> Result<u64, String> {
    let parsed = value.parse::<u64>().map_err(|error| {
        format!(
            "{}:{line_number}: invalid integer for {key}: {error}",
            path.display()
        )
    })?;
    if parsed == 0 {
        return Err(format!(
            "{}:{line_number}: {key} must be greater than zero",
            path.display()
        ));
    }
    Ok(parsed)
}

fn parse_positive_u64_env(name: &str, value: &str) -> Result<u64, String> {
    let parsed = value
        .parse::<u64>()
        .map_err(|error| format!("{name}: invalid integer: {error}"))?;
    if parsed == 0 {
        return Err(format!("{name}: must be greater than zero"));
    }
    Ok(parsed)
}

fn parse_positive_u32(
    path: &Path,
    line_number: usize,
    key: &str,
    value: &str,
) -> Result<u32, String> {
    let parsed = value.parse::<u32>().map_err(|error| {
        format!(
            "{}:{line_number}: invalid integer for {key}: {error}",
            path.display()
        )
    })?;
    if parsed == 0 {
        return Err(format!(
            "{}:{line_number}: {key} must be greater than zero",
            path.display()
        ));
    }
    Ok(parsed)
}

fn parse_positive_u32_env(name: &str, value: &str) -> Result<u32, String> {
    let parsed = value
        .parse::<u32>()
        .map_err(|error| format!("{name}: invalid integer: {error}"))?;
    if parsed == 0 {
        return Err(format!("{name}: must be greater than zero"));
    }
    Ok(parsed)
}

fn parse_worker_count(
    path: &Path,
    line_number: usize,
    key: &str,
    value: &str,
) -> Result<usize, String> {
    Ok(clamp_worker_count(parse_positive_usize(
        path,
        line_number,
        key,
        value,
    )?))
}

fn parse_worker_count_env(name: &str, value: &str) -> Result<usize, String> {
    parse_positive_usize_env(name, value).map(clamp_worker_count)
}

fn parse_positive_usize_env(name: &str, value: &str) -> Result<usize, String> {
    let parsed = value
        .parse::<usize>()
        .map_err(|error| format!("{name}: invalid integer: {error}"))?;
    if parsed == 0 {
        return Err(format!("{name}: must be greater than zero"));
    }
    Ok(parsed)
}

fn clamp_worker_count(value: usize) -> usize {
    let available = std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get);
    value.clamp(1, available.clamp(1, 32))
}

const fn mib_to_bytes(mib: usize) -> usize {
    mib.saturating_mul(MIB_BYTES)
}

fn parse_canvas_tile_count_env(name: &str, value: &str) -> Result<RawCanvasTileCount, String> {
    if value == "auto" {
        return Ok(RawCanvasTileCount::Auto);
    }
    let parsed = value
        .parse::<usize>()
        .map_err(|error| format!("{name}: invalid integer: {error}"))?;
    if parsed == 0 {
        return Err(format!("{name}: must be greater than zero"));
    }
    if parsed > MAX_CANVAS_TILE_COUNT {
        return Err(format!("{name}: must be at most {MAX_CANVAS_TILE_COUNT}"));
    }
    Ok(RawCanvasTileCount::Fixed(parsed))
}

fn parse_canvas_max_tile_count_env(name: &str, value: &str) -> Result<usize, String> {
    let parsed = value
        .parse::<usize>()
        .map_err(|error| format!("{name}: invalid integer: {error}"))?;
    if parsed == 0 {
        return Err(format!("{name}: must be greater than zero"));
    }
    if parsed > MAX_CANVAS_TILE_COUNT {
        return Err(format!("{name}: must be at most {MAX_CANVAS_TILE_COUNT}"));
    }
    Ok(parsed)
}

fn parse_canvas_overview_scale_env(name: &str, value: &str) -> Result<f32, String> {
    let parsed = value
        .parse::<f32>()
        .map_err(|error| format!("{name}: invalid number: {error}"))?;
    if !parsed.is_finite() || parsed <= 0.0 || parsed > 1.0 {
        return Err(format!("{name}: must be greater than 0 and at most 1"));
    }
    Ok(parsed)
}

fn parse_canvas_pan_axis_env(name: &str, value: &str) -> Result<CanvasPanAxis, String> {
    parse_canvas_pan_axis(value).map_err(|error| format!("{name}: {error}"))
}

fn config_error(path: &Path, line_number: usize, error: impl std::fmt::Display) -> String {
    format!("{}:{line_number}: {error}", path.display())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use mural_ipc::{CanvasPanAxis, Easing, PushDirection};

    use super::*;

    fn temp_config_path(name: &str) -> PathBuf {
        let dir = env::temp_dir().join(format!("mural-config-test-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir.join("config")
    }

    #[test]
    fn config_path_ignores_empty_or_relative_xdg_home() {
        let home = OsString::from("/home/test");
        let fallback = Some(PathBuf::from("/home/test/.config/mural/config"));

        for xdg_home in [Some(OsString::new()), Some(OsString::from("relative"))] {
            assert_eq!(
                config_path_from(None, xdg_home, Some(home.clone())),
                fallback
            );
        }
        assert_eq!(
            config_path_from(None, Some(OsString::from("/tmp/xdg-config")), Some(home)),
            Some(PathBuf::from("/tmp/xdg-config/mural/config"))
        );
    }

    #[test]
    fn explicit_mural_config_still_takes_precedence() {
        assert_eq!(
            config_path_from(
                Some(OsString::from("relative/explicit-config")),
                Some(OsString::from("/tmp/xdg-config")),
                Some(OsString::from("/home/test"))
            ),
            Some(PathBuf::from("relative/explicit-config"))
        );
    }

    #[test]
    fn parses_config_file_and_builds_action_transitions() {
        let path = temp_config_path("parse");
        fs::write(
            &path,
            r"
                wall_dir = ~/Pictures/walls
                state_dir = /tmp/mural-state
                favorite_weight = 6
                scale_mode = fit
                transition.push.duration_ms = 120
                transition.push.easing = linear
                transition.push.mode = screen
                transition.canvas.zoom_out_ms = 111
                transition.canvas.pan_ms = 22
                transition.canvas.zoom_in_ms = 333
                transition.canvas.easing = linear
                transition.canvas.mode = overlap
                transition.canvas.walk = strip
                transition.canvas.pan_axis = horizontal
                transition.canvas.overview_scale = 0.25
                transition.canvas.tile_count = 9
                transition.canvas.max_tile_count = 12
                canvas.thumbnail_max_edge = 1536
                canvas.cache.backend = internal
                canvas.cache.workers = 4
                canvas.cache.memory_mib = 768
                decode.full_workers = 3
                action.next = push:left
                action.back = canvas
                action.replace = cut
            ",
        )
        .unwrap();

        let mut raw = RawConfig::default();
        raw.apply_file(&path).unwrap();
        let config = raw.build().unwrap();

        assert_eq!(config.favorite_weight, Some(6));
        assert_eq!(config.scale_mode, ScaleMode::Fit);
        assert_eq!(config.canvas_thumbnail_max_edge, 1536);
        assert_eq!(config.canvas_cache_backend, CacheBackend::Internal);
        assert_eq!(config.decode_full_workers, clamp_worker_count(3));
        assert_eq!(config.canvas_cache_workers, clamp_worker_count(4));
        assert_eq!(config.canvas_cache_memory_bytes, 768 * MIB_BYTES);
        assert!(matches!(
            config.transition_for_action(&WallpaperAction::Next),
            Transition::Push {
                direction: PushDirection::Left,
                duration_ms: 120,
                easing: Easing::Linear,
                mode: PushMode::Screen,
            }
        ));
        assert!(matches!(
            config.transition_for_action(&WallpaperAction::Back),
            Transition::Canvas {
                zoom_out_ms: 111,
                pan_ms: 22,
                zoom_in_ms: 333,
                easing: Easing::Linear,
                mode: CanvasMode::Overlap,
                walk: CanvasWalk::Strip,
                pan_axis: CanvasPanAxis::Horizontal,
                overview_scale: 0.25,
                tile_count: CanvasTileCount::Fixed(9),
            }
        ));
        assert_eq!(
            config.transition_for_action(&WallpaperAction::Replace { index: 0 }),
            Transition::Cut
        );
    }

    #[test]
    fn rejects_unknown_keys() {
        let path = temp_config_path("unknown");
        fs::write(&path, "mystery = value\n").unwrap();

        let mut raw = RawConfig::default();
        assert!(
            raw.apply_file(&path)
                .unwrap_err()
                .contains("unknown config key")
        );
    }

    #[test]
    fn canvas_profile_settings_do_not_enable_canvas_when_unreferenced() {
        let path = temp_config_path("canvas-unreferenced");
        fs::write(
            &path,
            r"
                transition.canvas.zoom_out_ms = 111
                transition.canvas.pan_ms = 22
                transition.canvas.zoom_in_ms = 333
                transition.canvas.pan_axis = vertical
                transition.canvas.tile_count = 9
                action.next = push:up
                action.back = push:down
                action.shift_forward = push:left
                action.shift_back = push:right
                action.replace = cut
                action.quarantine = cut
                action.startup = cut
            ",
        )
        .unwrap();

        let mut raw = RawConfig::default();
        raw.apply_file(&path).unwrap();
        let config = raw.build().unwrap();

        assert!(!config.uses_canvas_transition());
        assert_eq!(config.canvas_prewarm_transition(), None);
    }

    #[test]
    fn canvas_profile_is_enabled_only_when_action_references_it() {
        let path = temp_config_path("canvas-referenced");
        fs::write(
            &path,
            r"
                transition.canvas.overview_scale = 0.25
                transition.canvas.tile_count = auto
                transition.canvas.max_tile_count = 9
                action.next = canvas
            ",
        )
        .unwrap();

        let mut raw = RawConfig::default();
        raw.apply_file(&path).unwrap();
        let config = raw.build().unwrap();

        assert!(config.uses_canvas_transition());
        assert!(matches!(
            config.canvas_prewarm_transition(),
            Some(Transition::Canvas {
                overview_scale: 0.25,
                tile_count: CanvasTileCount::Auto { max: Some(9) },
                ..
            })
        ));
    }

    #[test]
    fn world_profile_is_enabled_only_when_action_references_it() {
        let path = temp_config_path("world-referenced");
        fs::write(
            &path,
            r"
                transition.world.duration_ms = 1400
                transition.world.easing = ease-in-out-cubic
                action.next = world
                action.back = push:down
                action.shift_forward = push:left
                action.shift_back = push:right
                action.replace = cut
                action.quarantine = cut
                action.startup = cut
            ",
        )
        .unwrap();

        let mut raw = RawConfig::default();
        raw.apply_file(&path).unwrap();
        let config = raw.build().unwrap();

        assert!(config.uses_world_transition());
        assert!(!config.uses_canvas_transition());
        assert_eq!(config.canvas_prewarm_transition(), None);
        assert_eq!(
            config.transition_for_action(&WallpaperAction::Next),
            Transition::World {
                duration_ms: 1400,
                easing: Easing::EaseInOutCubic,
            }
        );
    }

    #[test]
    fn world_fallback_accepts_cut_or_push_without_enabling_world() {
        let path = temp_config_path("world-fallback");
        fs::write(
            &path,
            r"
                transition.world.fallback = push:up
                action.next = push:down
            ",
        )
        .unwrap();

        let mut raw = RawConfig::default();
        raw.apply_file(&path).unwrap();
        let config = raw.build().unwrap();

        assert!(!config.uses_world_transition());
        assert_eq!(
            config.world_fallback_transition(),
            Some(Transition::Push {
                direction: PushDirection::Up,
                duration_ms: DEFAULT_DURATION_MS,
                easing: Easing::EaseOutCubic,
                mode: PushMode::Portal,
            })
        );
    }

    #[test]
    fn world_fallback_rejects_non_immediate_transitions() {
        let path = temp_config_path("world-fallback-reject");
        fs::write(
            &path,
            r"
                transition.world.fallback = canvas
            ",
        )
        .unwrap();

        let mut raw = RawConfig::default();
        raw.apply_file(&path).unwrap();
        let error = raw.build().unwrap_err();

        assert!(error.contains("transition.world.fallback must reference cut or push"));
    }

    #[test]
    fn named_push_profile_accepts_direction_override() {
        let path = temp_config_path("profile");
        fs::write(
            &path,
            r"
                transition.fast.type = push
                transition.fast.duration_ms = 55
                transition.fast.easing = ease-in-out-cubic
                transition.fast.mode = pan
                action.next = fast:up
                action.back = fast:down
            ",
        )
        .unwrap();

        let mut raw = RawConfig::default();
        raw.apply_file(&path).unwrap();
        let config = raw.build().unwrap();

        assert_eq!(
            config.transition_for_action(&WallpaperAction::Next),
            Transition::Push {
                direction: PushDirection::Up,
                duration_ms: 55,
                easing: Easing::EaseInOutCubic,
                mode: PushMode::Pan,
            }
        );
        assert!(matches!(
            config.transition_for_action(&WallpaperAction::Back),
            Transition::Push {
                direction: PushDirection::Down,
                duration_ms: 55,
                ..
            }
        ));
    }

    #[test]
    fn rejects_canvas_mode_on_push_profile() {
        let path = temp_config_path("push-collage");
        fs::write(
            &path,
            r"
                transition.fast.type = push
                transition.fast.mode = collage
                action.next = fast:up
            ",
        )
        .unwrap();

        let mut raw = RawConfig::default();
        raw.apply_file(&path).unwrap();
        let error = raw.build().unwrap_err();

        assert!(error.contains("mode 'collage' is not valid for push"));
    }

    #[test]
    fn rejects_paged_walk_for_canvas_collage_profile() {
        for mode in ["collage", "span"] {
            let path = temp_config_path(mode);
            fs::write(
                &path,
                format!(
                    r"
                        transition.canvas.mode = {mode}
                        action.next = canvas
                    "
                ),
            )
            .unwrap();

            let mut raw = RawConfig::default();
            raw.apply_file(&path).unwrap();
            let error = raw.build().unwrap_err();

            assert!(error.contains("requires canvas walk 'strip'"));
        }
    }

    #[test]
    fn rejects_push_mode_on_canvas_profile() {
        let path = temp_config_path("canvas-portal");
        fs::write(
            &path,
            r"
                transition.canvas.mode = portal
                action.next = canvas
            ",
        )
        .unwrap();

        let mut raw = RawConfig::default();
        let error = raw.apply_file(&path).unwrap_err();

        assert!(error.contains("unknown canvas mode: portal"));
    }

    #[test]
    fn canvas_profile_accepts_pan_axis_override() {
        let path = temp_config_path("canvas-axis");
        fs::write(
            &path,
            r"
                transition.canvas.pan_axis = vertical
                action.next = canvas:horizontal
                action.back = canvas
            ",
        )
        .unwrap();

        let mut raw = RawConfig::default();
        raw.apply_file(&path).unwrap();
        let config = raw.build().unwrap();

        assert!(matches!(
            config.transition_for_action(&WallpaperAction::Next),
            Transition::Canvas {
                pan_axis: CanvasPanAxis::Horizontal,
                ..
            }
        ));
        assert!(matches!(
            config.transition_for_action(&WallpaperAction::Back),
            Transition::Canvas {
                pan_axis: CanvasPanAxis::Vertical,
                ..
            }
        ));
    }

    #[test]
    fn named_fade_profile_resolves_duration_and_easing() {
        let path = temp_config_path("fade-profile");
        fs::write(
            &path,
            r"
                transition.soft.type = fade
                transition.soft.duration_ms = 640
                transition.soft.easing = linear
                action.next = soft
            ",
        )
        .unwrap();

        let mut raw = RawConfig::default();
        raw.apply_file(&path).unwrap();
        let config = raw.build().unwrap();

        assert_eq!(
            config.transition_for_action(&WallpaperAction::Next),
            Transition::Fade {
                duration_ms: 640,
                easing: Easing::Linear,
            }
        );
    }

    #[test]
    fn invalid_fade_fields_are_rejected_even_when_profile_is_unreferenced() {
        let path = temp_config_path("fade-invalid-field");
        fs::write(
            &path,
            r"
                transition.unused.type = fade
                transition.unused.direction = left
            ",
        )
        .unwrap();

        let mut raw = RawConfig::default();
        raw.apply_file(&path).unwrap();
        let error = raw.build().unwrap_err();

        assert_eq!(
            error,
            "transition profile 'unused' field 'direction' is not valid for fade"
        );
    }

    #[test]
    fn unreferenced_named_profile_still_requires_a_compiled_in_type() {
        let path = temp_config_path("profile-missing-type");
        fs::write(&path, "transition.unused.duration_ms = 250\n").unwrap();

        let mut raw = RawConfig::default();
        raw.apply_file(&path).unwrap();
        let error = raw.build().unwrap_err();

        assert!(error.contains("transition profile 'unused' must declare a compiled-in type"));
    }

    #[test]
    fn invalid_unreferenced_push_mode_is_rejected() {
        let path = temp_config_path("unused-push-mode");
        fs::write(
            &path,
            r"
                transition.unused.type = push
                transition.unused.mode = collage
            ",
        )
        .unwrap();

        let mut raw = RawConfig::default();
        raw.apply_file(&path).unwrap();
        let error = raw.build().unwrap_err();

        assert_eq!(
            error,
            "transition profile 'unused' mode 'collage' is not valid for push; expected portal, screen, or pan"
        );
    }

    #[test]
    fn invalid_unreferenced_canvas_mode_walk_pair_is_rejected() {
        let path = temp_config_path("unused-canvas-mode-walk");
        fs::write(
            &path,
            r"
                transition.unused.type = canvas
                transition.unused.mode = collage
                transition.unused.walk = paged
            ",
        )
        .unwrap();

        let mut raw = RawConfig::default();
        raw.apply_file(&path).unwrap();
        let error = raw.build().unwrap_err();

        assert!(error.contains("transition profile 'unused' is invalid"));
        assert!(error.contains("canvas mode 'collage' requires canvas walk 'strip'"));
    }

    #[test]
    fn built_in_transition_profiles_cannot_be_retyped() {
        let path = temp_config_path("retyped-builtin");
        fs::write(&path, "transition.fade.type = push\n").unwrap();

        let mut raw = RawConfig::default();
        raw.apply_file(&path).unwrap();
        let error = raw.build().unwrap_err();

        assert_eq!(
            error,
            "built-in transition profile 'fade' cannot be retyped"
        );
    }

    #[test]
    fn named_world_profile_cannot_define_the_global_fallback() {
        let path = temp_config_path("named-world-fallback");
        fs::write(
            &path,
            r"
                transition.orbit.type = world
                transition.orbit.fallback = cut
            ",
        )
        .unwrap();

        let mut raw = RawConfig::default();
        let error = raw.apply_file(&path).unwrap_err();

        assert!(error.contains("transition.orbit.fallback is only supported for world"));
    }
}
