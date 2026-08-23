#![allow(clippy::missing_errors_doc)]

//! Shared IPC protocol types and a small std-only JSON codec for mural.
//!
//! The protocol intentionally starts as line-agnostic JSON over a Unix socket:
//! one request per connection, one response per connection. This crate avoids
//! third-party dependencies so the initial daemon and CLI can build before the
//! Wayland/EGL stack is selected.

use std::collections::BTreeMap;
use std::env;
use std::fmt::{self, Write as _};
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;

mod json;
mod transition_registry;

use json::{JsonValue, parse_json};
pub use transition_registry::{
    TRANSITION_REGISTRY, TransitionClass, TransitionDescriptor, TransitionKind,
    TransitionParameterDefault, TransitionParameterDescriptor, TransitionParameterType,
    TransitionScopes, transition_descriptor, transition_registry,
};

/// Current protocol version emitted in ping responses.
pub const PROTOCOL_VERSION: u32 = 1;

/// Current schema version for capability discovery responses.
pub const CAPABILITIES_SCHEMA_VERSION: u32 = 1;

/// Default transition duration used by the CLI when a non-cut transition is
/// requested without an explicit duration.
pub const DEFAULT_DURATION_MS: u64 = 900;

/// Default zoom-out phase for the canvas transition.
pub const DEFAULT_CANVAS_OUT_MS: u64 = 180;

/// Default overview pan phase for the canvas transition.
pub const DEFAULT_CANVAS_PAN_MS: u64 = 80;

/// Default zoom-in phase for the canvas transition.
pub const DEFAULT_CANVAS_IN_MS: u64 = 260;

/// Default canvas overview tile scale as a fraction of the output size.
pub const DEFAULT_CANVAS_OVERVIEW_SCALE: f32 = 1.0 / 3.0;

/// Hard cap for canvas preview tiles.
pub const MAX_CANVAS_TILE_COUNT: usize = 256;

/// Default maximum edge for cached canvas thumbnails.
pub const DEFAULT_CANVAS_THUMBNAIL_MAX_EDGE: u32 = 1536;

/// Default number of cache workers requested by `muralctl cache warm`.
pub const DEFAULT_CANVAS_CACHE_WORKERS: usize = 1;

/// Public capability snapshot returned by a daemon.
#[derive(Clone, Debug, PartialEq)]
pub struct CapabilitiesResponse {
    pub schema_version: u32,
    pub protocol_version: u32,
    pub daemon_mode: DaemonMode,
    pub transitions: Vec<TransitionCapability>,
}

impl CapabilitiesResponse {
    /// Build a capability snapshot from the compiled-in transition registry.
    #[must_use]
    pub fn current(daemon_mode: DaemonMode) -> Self {
        Self {
            schema_version: CAPABILITIES_SCHEMA_VERSION,
            protocol_version: PROTOCOL_VERSION,
            daemon_mode,
            transitions: transition_registry()
                .iter()
                .map(|descriptor| TransitionCapability::from_descriptor(descriptor, daemon_mode))
                .collect(),
        }
    }
}

/// Stable daemon architecture mode that determines effective capabilities.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DaemonMode {
    Supervisor,
    Standalone,
}

impl DaemonMode {
    /// Stable capability string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Supervisor => "supervisor",
            Self::Standalone => "standalone",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "supervisor" => Some(Self::Supervisor),
            "standalone" => Some(Self::Standalone),
            _ => None,
        }
    }
}

/// Public capability data for one transition.
#[derive(Clone, Debug, PartialEq)]
pub struct TransitionCapability {
    pub name: String,
    pub class: TransitionClass,
    pub scopes: TransitionScopes,
    pub experimental: bool,
    pub requirements: Vec<String>,
    pub parameters: Vec<TransitionParameterCapability>,
}

impl TransitionCapability {
    fn from_descriptor(descriptor: &TransitionDescriptor, daemon_mode: DaemonMode) -> Self {
        let scopes =
            if daemon_mode == DaemonMode::Standalone && descriptor.kind == TransitionKind::World {
                TransitionScopes {
                    explicit_set: false,
                    wallpaper_actions: false,
                }
            } else {
                descriptor.scopes
            };

        Self {
            name: descriptor.name.to_owned(),
            class: descriptor.class,
            scopes,
            experimental: descriptor.experimental,
            requirements: descriptor
                .requirements
                .iter()
                .map(|requirement| (*requirement).to_owned())
                .collect(),
            parameters: descriptor
                .parameters
                .iter()
                .map(TransitionParameterCapability::from)
                .collect(),
        }
    }
}

/// Public capability data for one transition parameter.
#[derive(Clone, Debug, PartialEq)]
pub struct TransitionParameterCapability {
    pub name: String,
    pub value_type: TransitionParameterType,
    pub allowed_values: Vec<String>,
    pub required: bool,
    pub default_value: Option<TransitionParameterValue>,
    pub constraint: Option<String>,
}

/// Typed JSON value used for a transition parameter default.
#[derive(Clone, Debug, PartialEq)]
pub enum TransitionParameterValue {
    Integer(i64),
    Number(f64),
    String(String),
}

impl fmt::Display for TransitionParameterValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Integer(value) => write!(formatter, "{value}"),
            Self::Number(value) => write!(formatter, "{value}"),
            Self::String(value) => formatter.write_str(value),
        }
    }
}

impl From<&TransitionParameterDescriptor> for TransitionParameterCapability {
    fn from(descriptor: &TransitionParameterDescriptor) -> Self {
        Self {
            name: descriptor.name.to_owned(),
            value_type: descriptor.value_type,
            allowed_values: descriptor
                .allowed_values
                .iter()
                .map(|value| (*value).to_owned())
                .collect(),
            required: descriptor.required,
            default_value: descriptor.default_value.map(|value| match value {
                TransitionParameterDefault::Integer(value) => {
                    TransitionParameterValue::Integer(value)
                }
                TransitionParameterDefault::Number(value) => {
                    TransitionParameterValue::Number(value)
                }
                TransitionParameterDefault::String(value) => {
                    TransitionParameterValue::String(value.to_owned())
                }
            }),
            constraint: descriptor.constraint.map(ToOwned::to_owned),
        }
    }
}

/// A request sent from `muralctl` or another client to `murald`.
#[derive(Clone, Debug, PartialEq)]
pub enum Request {
    Ping,
    Capabilities,
    Health,
    Query,
    Set(SetRequest),
    Preload(PreloadRequest),
    Clear(ClearRequest),
    Wallpaper(WallpaperRequest),
    Cache(CacheRequest),
    RenderCanvasSet(RenderCanvasSetRequest),
    RenderWorldSet(RenderWorldSetRequest),
    Stop,
}

/// Set one or more outputs to explicit image paths.
#[derive(Clone, Debug, PartialEq)]
pub struct SetRequest {
    pub outputs: BTreeMap<String, String>,
    pub transition: Transition,
    pub scale_mode: ScaleMode,
    pub allow_partial: bool,
}

/// Internal renderer request for canvas sets planned by the supervisor.
#[derive(Clone, Debug, PartialEq)]
pub struct RenderCanvasSetRequest {
    pub outputs: BTreeMap<String, String>,
    pub transition: Transition,
    pub scale_mode: ScaleMode,
    pub allow_partial: bool,
    pub preview_paths: Vec<String>,
    pub preview_start: usize,
}

/// Internal renderer request for compact world-transition sets planned by the supervisor.
#[derive(Clone, Debug, PartialEq)]
pub struct RenderWorldSetRequest {
    pub outputs: BTreeMap<String, String>,
    pub transition: Transition,
    pub scale_mode: ScaleMode,
    pub allow_partial: bool,
    pub library_count: usize,
    pub columns: usize,
    pub fingerprint: u64,
    pub thumbnail_edge: u32,
    pub tile_cells: usize,
    pub routes: BTreeMap<String, WorldRouteFocus>,
}

/// Per-output route indices into the canonical row-major world.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WorldRouteFocus {
    pub current_index: usize,
    pub target_index: usize,
    pub lod: usize,
}

/// Decode/upload images without displaying them yet.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreloadRequest {
    pub outputs: BTreeMap<String, String>,
}

/// Clear outputs to a solid color.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClearRequest {
    pub outputs: Vec<String>,
    pub color: String,
}

/// High-level wallpaper-library action handled by `murald`.
#[derive(Clone, Debug, PartialEq)]
pub struct WallpaperRequest {
    pub action: WallpaperAction,
    pub transition: Option<Transition>,
    pub scale_mode: Option<ScaleMode>,
}

/// Canvas thumbnail cache request handled by `murald`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CacheRequest {
    pub action: CacheAction,
}

/// Canvas thumbnail cache action.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CacheAction {
    Status,
    Clear,
    Warm {
        scope: CacheWarmScope,
        workers: usize,
        backend: CacheBackend,
    },
}

/// Wallpaper set to warm into the canvas thumbnail cache.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CacheWarmScope {
    Current,
    All,
}

/// Thumbnail generator backend for canvas cache warming.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CacheBackend {
    Auto,
    Vips,
    Internal,
}

/// Native wallpaper control-plane action.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WallpaperAction {
    Next,
    Back,
    ShiftForward,
    ShiftBack,
    Replace { index: usize },
    Quarantine { index: usize },
    Favorite { index: usize },
    Unfavorite { index: usize },
    Favorites,
    Current,
    Rescan,
}

/// Transition requested for a set operation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Transition {
    Cut,
    Fade {
        duration_ms: u64,
        easing: Easing,
    },
    World {
        duration_ms: u64,
        easing: Easing,
    },
    Push {
        direction: PushDirection,
        duration_ms: u64,
        easing: Easing,
        mode: PushMode,
    },
    Canvas {
        zoom_out_ms: u64,
        pan_ms: u64,
        zoom_in_ms: u64,
        easing: Easing,
        mode: CanvasMode,
        walk: CanvasWalk,
        pan_axis: CanvasPanAxis,
        overview_scale: f32,
        tile_count: CanvasTileCount,
    },
}

/// Canvas tile-count policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CanvasTileCount {
    Auto { max: Option<usize> },
    Fixed(usize),
}

/// Axis used to lay out natural canvas order.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CanvasPanAxis {
    Auto,
    Horizontal,
    Vertical,
}

/// How canvas tile order is projected onto the preview grid.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CanvasWalk {
    Paged,
    Strip,
}

/// How canvas transitions lay out preview image content.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CanvasMode {
    Clipped,
    Morph,
    Overlap,
    Collage,
    Span,
}

/// Direction for a push transition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PushDirection {
    Up,
    Down,
    Left,
    Right,
}

/// How push transitions move image content.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PushMode {
    Portal,
    Screen,
    Pan,
}

/// Easing curve for animated transitions.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Easing {
    Linear,
    EaseOutCubic,
    EaseInOutCubic,
}

/// Wallpaper scale mode.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScaleMode {
    Fill,
    Fit,
    Center,
    Stretch,
}

/// A response returned by `murald`.
#[derive(Clone, Debug, PartialEq)]
pub enum Response {
    Pong { version: u32 },
    Capabilities(CapabilitiesResponse),
    Ack { message: String },
    Health(Box<HealthResponse>),
    Query(QueryResponse),
    Wallpaper(WallpaperResponse),
    Cache(CacheResponse),
    Error { message: String },
}

/// Query response containing current daemon state.
#[derive(Clone, Debug, PartialEq)]
pub struct QueryResponse {
    pub outputs: Vec<OutputState>,
}

/// Health response containing supervisor/renderer liveness state.
#[derive(Clone, Debug, PartialEq)]
pub struct HealthResponse {
    pub role: String,
    pub supervisor_pid: Option<u32>,
    pub renderer_pid: Option<u32>,
    pub renderer_generation: u64,
    pub renderer_state: String,
    pub restart_count: u64,
    pub last_error: Option<String>,
    pub last_diagnostic: Option<String>,
    pub outputs: Vec<HealthOutput>,
}

/// Renderer state known for one output in health responses.
#[derive(Clone, Debug, PartialEq)]
pub struct HealthOutput {
    pub name: String,
    pub layout_x: i32,
    pub layout_y: i32,
    pub width: i32,
    pub height: i32,
    pub power_state: String,
    pub render_state: String,
    pub restore_pending: bool,
    pub current_image: Option<String>,
    pub transition_target_image: Option<String>,
    pub scale_mode: ScaleMode,
    pub transition_state: TransitionState,
    pub queue_depth: usize,
    pub frame_callback_pending: bool,
    pub render_pending: bool,
}

/// Renderer state known for one output.
#[derive(Clone, Debug, PartialEq)]
pub struct OutputState {
    pub name: String,
    pub current_image: Option<String>,
    pub scale_mode: ScaleMode,
    pub transition_state: TransitionState,
    pub queue_depth: usize,
}

/// Current transition status for an output.
#[derive(Clone, Debug, PartialEq)]
pub enum TransitionState {
    Idle,
    Running { transition: Transition },
}

/// Response payload for high-level wallpaper-library actions.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WallpaperResponse {
    pub action: String,
    pub message: String,
    pub entries: Vec<WallpaperEntry>,
    pub favorites: Vec<String>,
}

/// Response payload for canvas thumbnail cache actions.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CacheResponse {
    pub action: String,
    pub message: String,
    pub ready: usize,
    pub pending: usize,
    pub scheduled: usize,
    pub failed: usize,
    pub backend: String,
}

/// One output/path row in a high-level wallpaper response.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WallpaperEntry {
    pub index: usize,
    pub output: String,
    pub favorite: bool,
    pub path: String,
}

/// Protocol parse/validation error.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProtocolError {
    message: String,
}

impl ProtocolError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    /// Human-readable error message.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for ProtocolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ProtocolError {}

impl PushDirection {
    /// Stable protocol string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Up => "up",
            Self::Down => "down",
            Self::Left => "left",
            Self::Right => "right",
        }
    }

    /// Parse a protocol string.
    pub fn parse(input: &str) -> Result<Self, ProtocolError> {
        match input {
            "up" => Ok(Self::Up),
            "down" => Ok(Self::Down),
            "left" => Ok(Self::Left),
            "right" => Ok(Self::Right),
            _ => Err(ProtocolError::new(format!(
                "unknown push direction: {input}"
            ))),
        }
    }
}

impl PushMode {
    /// Stable protocol string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Portal => "portal",
            Self::Screen => "screen",
            Self::Pan => "pan",
        }
    }

    /// Parse a protocol string.
    pub fn parse(input: &str) -> Result<Self, ProtocolError> {
        match input {
            "portal" => Ok(Self::Portal),
            "screen" => Ok(Self::Screen),
            "pan" => Ok(Self::Pan),
            _ => Err(ProtocolError::new(format!("unknown push mode: {input}"))),
        }
    }
}

impl Easing {
    /// Stable protocol string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Linear => "linear",
            Self::EaseOutCubic => "ease-out-cubic",
            Self::EaseInOutCubic => "ease-in-out-cubic",
        }
    }

    /// Parse a protocol string.
    pub fn parse(input: &str) -> Result<Self, ProtocolError> {
        match input {
            "linear" => Ok(Self::Linear),
            "ease-out-cubic" => Ok(Self::EaseOutCubic),
            "ease-in-out-cubic" => Ok(Self::EaseInOutCubic),
            _ => Err(ProtocolError::new(format!("unknown easing: {input}"))),
        }
    }
}

impl ScaleMode {
    /// Stable protocol string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Fill => "fill",
            Self::Fit => "fit",
            Self::Center => "center",
            Self::Stretch => "stretch",
        }
    }

    /// Parse a protocol string.
    pub fn parse(input: &str) -> Result<Self, ProtocolError> {
        match input {
            "fill" => Ok(Self::Fill),
            "fit" => Ok(Self::Fit),
            "center" => Ok(Self::Center),
            "stretch" => Ok(Self::Stretch),
            _ => Err(ProtocolError::new(format!("unknown scale mode: {input}"))),
        }
    }
}

impl CacheWarmScope {
    /// Stable protocol string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Current => "current",
            Self::All => "all",
        }
    }

    /// Parse a protocol string.
    pub fn parse(input: &str) -> Result<Self, ProtocolError> {
        match input {
            "current" => Ok(Self::Current),
            "all" => Ok(Self::All),
            _ => Err(ProtocolError::new(format!(
                "unknown cache warm scope: {input}"
            ))),
        }
    }
}

impl CacheBackend {
    /// Stable protocol string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Vips => "vips",
            Self::Internal => "internal",
        }
    }

    /// Parse a protocol string.
    pub fn parse(input: &str) -> Result<Self, ProtocolError> {
        match input {
            "auto" => Ok(Self::Auto),
            "vips" => Ok(Self::Vips),
            "internal" => Ok(Self::Internal),
            _ => Err(ProtocolError::new(format!(
                "unknown cache backend: {input}"
            ))),
        }
    }
}

impl WallpaperAction {
    /// Stable protocol action string.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Next => "next",
            Self::Back => "back",
            Self::ShiftForward => "shift-forward",
            Self::ShiftBack => "shift-back",
            Self::Replace { .. } => "replace",
            Self::Quarantine { .. } => "quarantine",
            Self::Favorite { .. } => "favorite",
            Self::Unfavorite { .. } => "unfavorite",
            Self::Favorites => "favorites",
            Self::Current => "current",
            Self::Rescan => "rescan",
        }
    }

    /// Parse an action string and optional index.
    pub fn parse(action: &str, index: Option<usize>) -> Result<Self, ProtocolError> {
        match action {
            "next" => Ok(Self::Next),
            "back" => Ok(Self::Back),
            "shift-forward" => Ok(Self::ShiftForward),
            "shift-back" => Ok(Self::ShiftBack),
            "replace" => Ok(Self::Replace {
                index: required_index(action, index)?,
            }),
            "quarantine" | "quarentine" => Ok(Self::Quarantine {
                index: required_index(action, index)?,
            }),
            "favorite" => Ok(Self::Favorite {
                index: required_index(action, index)?,
            }),
            "unfavorite" => Ok(Self::Unfavorite {
                index: required_index(action, index)?,
            }),
            "favorites" => Ok(Self::Favorites),
            "current" => Ok(Self::Current),
            "rescan" => Ok(Self::Rescan),
            _ => Err(ProtocolError::new(format!(
                "unknown wallpaper action: {action}"
            ))),
        }
    }
}

impl Transition {
    /// Build a transition from the compact CLI spelling.
    ///
    /// Accepted spellings are `cut`, `fade`, `world`, `push:up`, `push:down`,
    /// `push:left`, `push:right`, `canvas`, `canvas:auto`,
    /// `canvas:horizontal`, and `canvas:vertical`.
    pub fn parse_cli_token(
        input: &str,
        duration_ms: u64,
        easing: Easing,
    ) -> Result<Self, ProtocolError> {
        let (name, suffix) = input
            .split_once(':')
            .map_or((input, None), |(name, suffix)| (name, Some(suffix)));
        let descriptor = transition_descriptor(name)
            .ok_or_else(|| ProtocolError::new(format!("unknown transition: {input}")))?;

        match descriptor.kind {
            TransitionKind::Cut => {
                if suffix.is_some() {
                    return Err(ProtocolError::new(format!(
                        "transition '{name}' does not accept ':override'"
                    )));
                }
                Ok(Self::Cut)
            }
            TransitionKind::Fade => {
                if suffix.is_some() {
                    return Err(ProtocolError::new(format!(
                        "transition '{name}' does not accept ':override'"
                    )));
                }
                validate_positive_milliseconds(duration_ms, "duration_ms")?;
                Ok(Self::Fade {
                    duration_ms,
                    easing,
                })
            }
            TransitionKind::World => {
                if suffix.is_some() {
                    return Err(ProtocolError::new(format!(
                        "transition '{name}' does not accept ':override'"
                    )));
                }
                validate_positive_milliseconds(duration_ms, "duration_ms")?;
                Ok(Self::World {
                    duration_ms,
                    easing,
                })
            }
            TransitionKind::Push => {
                validate_positive_milliseconds(duration_ms, "duration_ms")?;
                Ok(Self::Push {
                    direction: PushDirection::parse(suffix.ok_or_else(|| {
                        ProtocolError::new("push transition requires a direction override")
                    })?)?,
                    duration_ms,
                    easing,
                    mode: PushMode::Portal,
                })
            }
            TransitionKind::Canvas => {
                let pan_axis = suffix.map_or(Ok(CanvasPanAxis::Auto), CanvasPanAxis::parse)?;
                Ok(Self::Canvas {
                    zoom_out_ms: DEFAULT_CANVAS_OUT_MS,
                    pan_ms: DEFAULT_CANVAS_PAN_MS,
                    zoom_in_ms: DEFAULT_CANVAS_IN_MS,
                    easing,
                    mode: CanvasMode::Clipped,
                    walk: CanvasWalk::Paged,
                    pan_axis,
                    overview_scale: DEFAULT_CANVAS_OVERVIEW_SCALE,
                    tile_count: CanvasTileCount::Auto { max: None },
                })
            }
        }
    }
}

impl CanvasPanAxis {
    /// Stable protocol string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Horizontal => "horizontal",
            Self::Vertical => "vertical",
        }
    }

    /// Parse a protocol string.
    pub fn parse(input: &str) -> Result<Self, ProtocolError> {
        match input {
            "auto" => Ok(Self::Auto),
            "horizontal" => Ok(Self::Horizontal),
            "vertical" => Ok(Self::Vertical),
            _ => Err(ProtocolError::new(format!(
                "unknown canvas pan axis: {input}"
            ))),
        }
    }
}

impl CanvasWalk {
    /// Stable protocol string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Paged => "paged",
            Self::Strip => "strip",
        }
    }

    /// Parse a protocol string.
    pub fn parse(input: &str) -> Result<Self, ProtocolError> {
        match input {
            "paged" | "page" | "grid" => Ok(Self::Paged),
            "strip" | "line" => Ok(Self::Strip),
            _ => Err(ProtocolError::new(format!("unknown canvas walk: {input}"))),
        }
    }
}

impl CanvasMode {
    /// Stable protocol string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Clipped => "clipped",
            Self::Morph => "morph",
            Self::Overlap => "overlap",
            Self::Collage => "collage",
            Self::Span => "span",
        }
    }

    /// Parse a protocol string.
    pub fn parse(input: &str) -> Result<Self, ProtocolError> {
        match input {
            "clipped" => Ok(Self::Clipped),
            "morph" => Ok(Self::Morph),
            "overlap" => Ok(Self::Overlap),
            "collage" => Ok(Self::Collage),
            "span" => Ok(Self::Span),
            _ => Err(ProtocolError::new(format!("unknown canvas mode: {input}"))),
        }
    }
}

/// Validate a canvas mode/walk pair.
pub fn validate_canvas_mode_walk(mode: CanvasMode, walk: CanvasWalk) -> Result<(), ProtocolError> {
    if matches!(
        (mode, walk),
        (CanvasMode::Collage | CanvasMode::Span, CanvasWalk::Paged)
    ) {
        return Err(ProtocolError::new(format!(
            "canvas mode '{}' requires canvas walk 'strip'; 'paged' is not valid for this layout",
            mode.as_str()
        )));
    }
    Ok(())
}

impl Request {
    /// Serialize this request as compact JSON.
    #[must_use]
    pub fn to_json(&self) -> String {
        match self {
            Self::Ping => object([("type", json_string("ping"))]),
            Self::Capabilities => object([("type", json_string("capabilities"))]),
            Self::Health => object([("type", json_string("health"))]),
            Self::Query => object([("type", json_string("query"))]),
            Self::Stop => object([("type", json_string("stop"))]),
            Self::Set(request) => {
                let fields = vec![
                    ("type", json_string("set")),
                    ("outputs", encode_string_map(&request.outputs)),
                    ("transition", encode_transition(request.transition)),
                    ("scale_mode", json_string(request.scale_mode.as_str())),
                    ("allow_partial", request.allow_partial.to_string()),
                ];
                object(fields)
            }
            Self::Preload(request) => object([
                ("type", json_string("preload")),
                ("outputs", encode_string_map(&request.outputs)),
            ]),
            Self::Clear(request) => object([
                ("type", json_string("clear")),
                ("outputs", encode_string_array(&request.outputs)),
                ("color", json_string(&request.color)),
            ]),
            Self::Wallpaper(request) => object([
                ("type", json_string("wallpaper")),
                ("action", encode_wallpaper_action(&request.action)),
                (
                    "transition",
                    request
                        .transition
                        .map_or_else(|| "null".to_owned(), encode_transition),
                ),
                (
                    "scale_mode",
                    request.scale_mode.map_or_else(
                        || "null".to_owned(),
                        |scale_mode| json_string(scale_mode.as_str()),
                    ),
                ),
            ]),
            Self::Cache(request) => {
                let mut fields = vec![("type", json_string("cache"))];
                match &request.action {
                    CacheAction::Status => {
                        fields.push(("action", json_string("status")));
                    }
                    CacheAction::Clear => {
                        fields.push(("action", json_string("clear")));
                    }
                    CacheAction::Warm {
                        scope,
                        workers,
                        backend,
                    } => {
                        fields.push(("action", json_string("warm")));
                        fields.push(("scope", json_string(scope.as_str())));
                        fields.push(("workers", workers.to_string()));
                        fields.push(("backend", json_string(backend.as_str())));
                    }
                }
                object(fields)
            }
            Self::RenderCanvasSet(request) => object([
                ("type", json_string("renderer_canvas_set")),
                ("outputs", encode_string_map(&request.outputs)),
                ("transition", encode_transition(request.transition)),
                ("scale_mode", json_string(request.scale_mode.as_str())),
                ("allow_partial", request.allow_partial.to_string()),
                ("preview_paths", encode_string_array(&request.preview_paths)),
                ("preview_start", request.preview_start.to_string()),
            ]),
            Self::RenderWorldSet(request) => object([
                ("type", json_string("renderer_world_set")),
                ("outputs", encode_string_map(&request.outputs)),
                ("transition", encode_transition(request.transition)),
                ("scale_mode", json_string(request.scale_mode.as_str())),
                ("allow_partial", request.allow_partial.to_string()),
                ("library_count", request.library_count.to_string()),
                ("columns", request.columns.to_string()),
                ("fingerprint", request.fingerprint.to_string()),
                ("thumbnail_edge", request.thumbnail_edge.to_string()),
                ("tile_cells", request.tile_cells.to_string()),
                ("routes", encode_world_route_map(&request.routes)),
            ]),
        }
    }
}

impl Response {
    /// Serialize this response as compact JSON.
    #[must_use]
    pub fn to_json(&self) -> String {
        match self {
            Self::Pong { version } => object([
                ("status", json_string("ok")),
                ("type", json_string("pong")),
                ("version", version.to_string()),
            ]),
            Self::Capabilities(capabilities) => object([
                ("status", json_string("ok")),
                ("type", json_string("capabilities")),
                ("schema_version", capabilities.schema_version.to_string()),
                (
                    "protocol_version",
                    capabilities.protocol_version.to_string(),
                ),
                (
                    "daemon_mode",
                    json_string(capabilities.daemon_mode.as_str()),
                ),
                (
                    "transitions",
                    encode_transition_capabilities(&capabilities.transitions),
                ),
            ]),
            Self::Ack { message } => object([
                ("status", json_string("ok")),
                ("type", json_string("ack")),
                ("message", json_string(message)),
            ]),
            Self::Health(health) => object([
                ("status", json_string("ok")),
                ("type", json_string("health")),
                ("role", json_string(&health.role)),
                ("supervisor_pid", encode_optional_u32(health.supervisor_pid)),
                ("renderer_pid", encode_optional_u32(health.renderer_pid)),
                (
                    "renderer_generation",
                    health.renderer_generation.to_string(),
                ),
                ("renderer_state", json_string(&health.renderer_state)),
                ("restart_count", health.restart_count.to_string()),
                (
                    "last_error",
                    health
                        .last_error
                        .as_deref()
                        .map_or_else(|| "null".to_owned(), json_string),
                ),
                (
                    "last_diagnostic",
                    health
                        .last_diagnostic
                        .as_deref()
                        .map_or_else(|| "null".to_owned(), json_string),
                ),
                ("outputs", encode_health_outputs(&health.outputs)),
            ]),
            Self::Query(query) => object([
                ("status", json_string("ok")),
                ("type", json_string("query")),
                ("outputs", encode_outputs(&query.outputs)),
            ]),
            Self::Wallpaper(response) => object([
                ("status", json_string("ok")),
                ("type", json_string("wallpaper")),
                ("action", json_string(&response.action)),
                ("message", json_string(&response.message)),
                ("entries", encode_wallpaper_entries(&response.entries)),
                ("favorites", encode_string_array(&response.favorites)),
            ]),
            Self::Cache(response) => object([
                ("status", json_string("ok")),
                ("type", json_string("cache")),
                ("action", json_string(&response.action)),
                ("message", json_string(&response.message)),
                ("ready", response.ready.to_string()),
                ("pending", response.pending.to_string()),
                ("scheduled", response.scheduled.to_string()),
                ("failed", response.failed.to_string()),
                ("backend", json_string(&response.backend)),
            ]),
            Self::Error { message } => object([
                ("status", json_string("error")),
                ("message", json_string(message)),
            ]),
        }
    }
}

/// Return the default runtime socket path.
///
/// This uses `$MURAL_SOCKET` first for testing and ad-hoc sessions, then
/// `$XDG_RUNTIME_DIR/mural/mural.sock`, and finally an effective-UID-scoped path
/// under `/tmp` as a last resort for non-systemd test environments.
pub fn default_socket_path() -> Result<PathBuf, String> {
    if let Some(socket) = env::var_os("MURAL_SOCKET").filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(socket));
    }

    if let Some(runtime_dir) = env::var_os("XDG_RUNTIME_DIR").filter(|value| !value.is_empty()) {
        let runtime_dir = PathBuf::from(runtime_dir);
        if runtime_dir.is_absolute() {
            return Ok(runtime_dir.join("mural/mural.sock"));
        }
    }

    fallback_socket_path_for_uid(
        fs::metadata("/proc/self")
            .ok()
            .map(|metadata| metadata.uid()),
    )
}

fn fallback_socket_path_for_uid(uid: Option<u32>) -> Result<PathBuf, String> {
    let uid = uid.ok_or_else(|| {
        "cannot determine the effective UID for the fallback mural socket; set MURAL_SOCKET or XDG_RUNTIME_DIR"
            .to_owned()
    })?;
    Ok(PathBuf::from("/tmp").join(format!("mural-{uid}/mural.sock")))
}

/// Parse a request from JSON.
pub fn parse_request(input: &str) -> Result<Request, ProtocolError> {
    parse_request_with_renderer_control(input, true)
}

/// Parse a request received on a public daemon socket.
///
/// Renderer planning requests are reserved for the inherited supervisor-to-renderer
/// control channel and are rejected here even when their remaining fields are valid.
pub fn parse_public_request(input: &str) -> Result<Request, ProtocolError> {
    parse_request_with_renderer_control(input, false)
}

fn parse_request_with_renderer_control(
    input: &str,
    allow_renderer_control: bool,
) -> Result<Request, ProtocolError> {
    let json = parse_json(input)?;
    let object = json.as_object()?;
    let request_type = required_string(object, "type")?;

    match request_type {
        "ping" => Ok(Request::Ping),
        "capabilities" => Ok(Request::Capabilities),
        "health" => Ok(Request::Health),
        "query" => Ok(Request::Query),
        "stop" => Ok(Request::Stop),
        "set" => parse_set_request(object).map(Request::Set),
        "preload" => parse_preload_request(object).map(Request::Preload),
        "clear" => parse_clear_request(object).map(Request::Clear),
        "wallpaper" => parse_wallpaper_request(object).map(Request::Wallpaper),
        "cache" => parse_cache_request(object).map(Request::Cache),
        "renderer_canvas_set" if allow_renderer_control => {
            parse_render_canvas_set_request(object).map(Request::RenderCanvasSet)
        }
        "renderer_world_set" if allow_renderer_control => {
            parse_render_world_set_request(object).map(Request::RenderWorldSet)
        }
        "renderer_canvas_set" | "renderer_world_set" => Err(ProtocolError::new(format!(
            "request type is reserved for the internal renderer control channel: {request_type}"
        ))),
        _ => Err(ProtocolError::new(format!(
            "unknown request type: {request_type}"
        ))),
    }
}

/// Return true if a response JSON string has error status.
///
/// This is intentionally small and tolerant because the CLI only needs to set a
/// non-zero exit status after printing the daemon's raw response.
#[must_use]
pub fn response_is_error(response_json: &str) -> bool {
    parse_json(response_json)
        .ok()
        .and_then(|json| json.into_object().ok())
        .and_then(|mut object| object.remove("status"))
        .and_then(|value| value.into_string().ok())
        .is_some_and(|status| status == "error")
}

/// Parse the health response emitted by `murald`.
pub fn parse_health_response(input: &str) -> Result<HealthResponse, ProtocolError> {
    let json = parse_json(input)?;
    let object = json.as_object()?;
    match required_string(object, "status")? {
        "ok" => {}
        "error" => {
            return Err(ProtocolError::new(
                optional_string(object, "message")?
                    .unwrap_or("health request failed")
                    .to_owned(),
            ));
        }
        status => {
            return Err(ProtocolError::new(format!(
                "unknown response status: {status}"
            )));
        }
    }

    let response_type = required_string(object, "type")?;
    if response_type != "health" {
        return Err(ProtocolError::new(format!(
            "expected health response, got {response_type}"
        )));
    }

    let outputs = match object.get("outputs") {
        Some(JsonValue::Array(values)) => values
            .iter()
            .map(parse_health_output)
            .collect::<Result<Vec<_>, _>>()?,
        Some(_) => return Err(ProtocolError::new("field 'outputs' must be an array")),
        None => Vec::new(),
    };

    Ok(HealthResponse {
        role: required_string(object, "role")?.to_owned(),
        supervisor_pid: optional_u32(object, "supervisor_pid")?,
        renderer_pid: optional_u32(object, "renderer_pid")?,
        renderer_generation: optional_u64(object, "renderer_generation")?.unwrap_or(0),
        renderer_state: optional_string(object, "renderer_state")?
            .unwrap_or("unknown")
            .to_owned(),
        restart_count: optional_u64(object, "restart_count")?.unwrap_or(0),
        last_error: optional_nullable_string(object, "last_error")?,
        last_diagnostic: optional_nullable_string(object, "last_diagnostic")?,
        outputs,
    })
}

/// Parse the capability response emitted by `murald`.
pub fn parse_capabilities_response(input: &str) -> Result<CapabilitiesResponse, ProtocolError> {
    let json = parse_json(input)?;
    let object = json.as_object()?;
    match required_string(object, "status")? {
        "ok" => {}
        "error" => {
            return Err(ProtocolError::new(
                optional_string(object, "message")?
                    .unwrap_or("capabilities request failed")
                    .to_owned(),
            ));
        }
        status => {
            return Err(ProtocolError::new(format!(
                "unknown response status: {status}"
            )));
        }
    }

    let response_type = required_string(object, "type")?;
    if response_type != "capabilities" {
        return Err(ProtocolError::new(format!(
            "expected capabilities response, got {response_type}"
        )));
    }

    let schema_version = optional_u32(object, "schema_version")?
        .ok_or_else(|| ProtocolError::new("missing required field 'schema_version'"))?;
    if schema_version != CAPABILITIES_SCHEMA_VERSION {
        return Err(ProtocolError::new(format!(
            "unsupported capabilities schema version {schema_version}; supported version is {CAPABILITIES_SCHEMA_VERSION}"
        )));
    }
    let protocol_version = optional_u32(object, "protocol_version")?
        .ok_or_else(|| ProtocolError::new("missing required field 'protocol_version'"))?;
    let daemon_mode_name = required_string(object, "daemon_mode")?;
    let daemon_mode = DaemonMode::parse(daemon_mode_name)
        .ok_or_else(|| ProtocolError::new(format!("unknown daemon mode: {daemon_mode_name}")))?;
    let transitions = match object.get("transitions") {
        Some(JsonValue::Array(values)) => values
            .iter()
            .map(parse_transition_capability)
            .collect::<Result<Vec<_>, _>>()?,
        Some(_) => return Err(ProtocolError::new("field 'transitions' must be an array")),
        None => return Err(ProtocolError::new("missing required field 'transitions'")),
    };

    Ok(CapabilitiesResponse {
        schema_version,
        protocol_version,
        daemon_mode,
        transitions,
    })
}

fn parse_transition_capability(value: &JsonValue) -> Result<TransitionCapability, ProtocolError> {
    let object = value.as_object()?;
    let class_name = required_string(object, "class")?;
    let class = TransitionClass::parse(class_name)
        .ok_or_else(|| ProtocolError::new(format!("unknown transition class: {class_name}")))?;
    let scopes = object
        .get("scopes")
        .ok_or_else(|| ProtocolError::new("missing required field 'scopes'"))?
        .as_object()?;
    let parameters = match object.get("parameters") {
        Some(JsonValue::Array(values)) => values
            .iter()
            .map(parse_transition_parameter_capability)
            .collect::<Result<Vec<_>, _>>()?,
        Some(_) => return Err(ProtocolError::new("field 'parameters' must be an array")),
        None => Vec::new(),
    };
    let requirements = match object.get("requirements") {
        Some(JsonValue::Array(values)) => values
            .iter()
            .map(|value| value.as_string().map(ToOwned::to_owned))
            .collect::<Result<Vec<_>, _>>()?,
        Some(_) => {
            return Err(ProtocolError::new("field 'requirements' must be an array"));
        }
        None => Vec::new(),
    };

    Ok(TransitionCapability {
        name: required_string(object, "name")?.to_owned(),
        class,
        scopes: TransitionScopes {
            explicit_set: optional_bool(scopes, "explicit_set")?.unwrap_or(false),
            wallpaper_actions: optional_bool(scopes, "wallpaper_actions")?.unwrap_or(false),
        },
        experimental: optional_bool(object, "experimental")?.unwrap_or(false),
        requirements,
        parameters,
    })
}

fn parse_transition_parameter_capability(
    value: &JsonValue,
) -> Result<TransitionParameterCapability, ProtocolError> {
    let object = value.as_object()?;
    let type_name = required_string(object, "type")?;
    let value_type = TransitionParameterType::parse(type_name).ok_or_else(|| {
        ProtocolError::new(format!("unknown transition parameter type: {type_name}"))
    })?;
    let allowed_values = match object.get("allowed_values") {
        Some(JsonValue::Array(values)) => values
            .iter()
            .map(|value| value.as_string().map(ToOwned::to_owned))
            .collect::<Result<Vec<_>, _>>()?,
        Some(_) => {
            return Err(ProtocolError::new(
                "field 'allowed_values' must be an array",
            ));
        }
        None => Vec::new(),
    };

    Ok(TransitionParameterCapability {
        name: required_string(object, "name")?.to_owned(),
        value_type,
        allowed_values,
        required: required_bool(object, "required")?,
        default_value: parse_transition_parameter_default(object, value_type)?,
        constraint: optional_nullable_string(object, "constraint")?,
    })
}

fn parse_transition_parameter_default(
    object: &BTreeMap<String, JsonValue>,
    value_type: TransitionParameterType,
) -> Result<Option<TransitionParameterValue>, ProtocolError> {
    let value = object
        .get("default")
        .ok_or_else(|| ProtocolError::new("missing required field 'default'"))?;
    let default =
        match value {
            JsonValue::Null => return Ok(None),
            JsonValue::String(value) => TransitionParameterValue::String(value.clone()),
            JsonValue::Number(value) => match value_type {
                TransitionParameterType::Integer | TransitionParameterType::IntegerOrEnum => {
                    TransitionParameterValue::Integer(value.parse::<i64>().map_err(|_| {
                        ProtocolError::new("integer field 'default' is out of range")
                    })?)
                }
                TransitionParameterType::Number => {
                    let value = value.parse::<f64>().map_err(|_| {
                        ProtocolError::new("field 'default' must be a finite JSON number")
                    })?;
                    if !value.is_finite() {
                        return Err(ProtocolError::new(
                            "field 'default' must be a finite JSON number",
                        ));
                    }
                    TransitionParameterValue::Number(value)
                }
                TransitionParameterType::Enum => {
                    return Err(ProtocolError::new(
                        "field 'default' does not match parameter type 'enum'",
                    ));
                }
            },
            _ => {
                return Err(ProtocolError::new(
                    "field 'default' must be an integer, number, string, or null",
                ));
            }
        };

    let type_matches = matches!(
        (&default, value_type),
        (
            TransitionParameterValue::Integer(_),
            TransitionParameterType::Integer | TransitionParameterType::IntegerOrEnum
        ) | (
            TransitionParameterValue::Number(_),
            TransitionParameterType::Number
        ) | (
            TransitionParameterValue::String(_),
            TransitionParameterType::Enum | TransitionParameterType::IntegerOrEnum
        )
    );
    if !type_matches {
        return Err(ProtocolError::new(format!(
            "field 'default' does not match parameter type '{}'",
            value_type.as_str()
        )));
    }

    Ok(Some(default))
}

/// Return an error response message, if the JSON is a protocol error response.
#[must_use]
pub fn response_error_message(response_json: &str) -> Option<String> {
    parse_json(response_json)
        .ok()
        .and_then(|json| json.into_object().ok())
        .and_then(|mut object| {
            let status = object.remove("status")?.into_string().ok()?;
            if status == "error" {
                object.remove("message")?.into_string().ok()
            } else {
                None
            }
        })
}

/// Parse the high-level wallpaper response emitted by `murald`.
pub fn parse_wallpaper_response(input: &str) -> Result<WallpaperResponse, ProtocolError> {
    let json = parse_json(input)?;
    let object = json.as_object()?;
    match required_string(object, "status")? {
        "ok" => {}
        "error" => {
            return Err(ProtocolError::new(
                optional_string(object, "message")?
                    .unwrap_or("wallpaper request failed")
                    .to_owned(),
            ));
        }
        status => {
            return Err(ProtocolError::new(format!(
                "unknown response status: {status}"
            )));
        }
    }

    let response_type = required_string(object, "type")?;
    if response_type != "wallpaper" {
        return Err(ProtocolError::new(format!(
            "expected wallpaper response, got {response_type}"
        )));
    }

    let entries = match object.get("entries") {
        Some(JsonValue::Array(values)) => values
            .iter()
            .map(parse_wallpaper_entry)
            .collect::<Result<Vec<_>, _>>()?,
        Some(_) => return Err(ProtocolError::new("field 'entries' must be an array")),
        None => Vec::new(),
    };
    let favorites = match object.get("favorites") {
        Some(JsonValue::Array(values)) => values
            .iter()
            .map(|value| value.as_string().map(ToOwned::to_owned))
            .collect::<Result<Vec<_>, _>>()?,
        Some(_) => return Err(ProtocolError::new("field 'favorites' must be an array")),
        None => Vec::new(),
    };

    Ok(WallpaperResponse {
        action: required_string(object, "action")?.to_owned(),
        message: optional_string(object, "message")?.unwrap_or("").to_owned(),
        entries,
        favorites,
    })
}

fn parse_set_request(object: &BTreeMap<String, JsonValue>) -> Result<SetRequest, ProtocolError> {
    let outputs = required_outputs(object)?;
    let transition = match object.get("transition") {
        Some(value) => parse_transition(value)?,
        None => Transition::Cut,
    };
    let scale_mode =
        optional_string(object, "scale_mode")?.map_or(Ok(ScaleMode::Fill), ScaleMode::parse)?;
    let allow_partial = optional_bool(object, "allow_partial")?.unwrap_or(false);

    Ok(SetRequest {
        outputs,
        transition,
        scale_mode,
        allow_partial,
    })
}

fn parse_preload_request(
    object: &BTreeMap<String, JsonValue>,
) -> Result<PreloadRequest, ProtocolError> {
    Ok(PreloadRequest {
        outputs: required_outputs(object)?,
    })
}

fn parse_clear_request(
    object: &BTreeMap<String, JsonValue>,
) -> Result<ClearRequest, ProtocolError> {
    let outputs = match object.get("outputs") {
        Some(JsonValue::Array(values)) => values
            .iter()
            .map(|value| value.as_string().map(ToOwned::to_owned))
            .collect::<Result<Vec<_>, _>>()?,
        Some(_) => return Err(ProtocolError::new("field 'outputs' must be an array")),
        None => Vec::new(),
    };

    let color = optional_string(object, "color")?
        .unwrap_or("#000000")
        .to_owned();
    Ok(ClearRequest { outputs, color })
}

fn parse_wallpaper_request(
    object: &BTreeMap<String, JsonValue>,
) -> Result<WallpaperRequest, ProtocolError> {
    let action = parse_wallpaper_action(
        object
            .get("action")
            .ok_or_else(|| ProtocolError::new("missing required field 'action'"))?,
    )?;
    let transition = match object.get("transition") {
        Some(JsonValue::Null) | None => None,
        Some(value) => Some(parse_transition(value)?),
    };
    let scale_mode = match object.get("scale_mode") {
        Some(JsonValue::Null) | None => None,
        Some(value) => Some(ScaleMode::parse(value.as_string()?)?),
    };

    Ok(WallpaperRequest {
        action,
        transition,
        scale_mode,
    })
}

fn parse_cache_request(
    object: &BTreeMap<String, JsonValue>,
) -> Result<CacheRequest, ProtocolError> {
    let action = required_string(object, "action")?;
    let action = match action {
        "status" => CacheAction::Status,
        "clear" => CacheAction::Clear,
        "warm" => {
            let scope = optional_string(object, "scope")?
                .map_or(Ok(CacheWarmScope::Current), CacheWarmScope::parse)?;
            let workers =
                optional_usize(object, "workers")?.unwrap_or(DEFAULT_CANVAS_CACHE_WORKERS);
            validate_cache_workers(workers)?;
            let backend = optional_string(object, "backend")?
                .map_or(Ok(CacheBackend::Auto), CacheBackend::parse)?;
            CacheAction::Warm {
                scope,
                workers,
                backend,
            }
        }
        _ => {
            return Err(ProtocolError::new(format!(
                "unknown cache action: {action}"
            )));
        }
    };

    Ok(CacheRequest { action })
}

fn parse_render_canvas_set_request(
    object: &BTreeMap<String, JsonValue>,
) -> Result<RenderCanvasSetRequest, ProtocolError> {
    let outputs = required_outputs(object)?;
    let transition = object
        .get("transition")
        .ok_or_else(|| ProtocolError::new("missing required field 'transition'"))
        .and_then(parse_transition)?;
    let scale_mode =
        optional_string(object, "scale_mode")?.map_or(Ok(ScaleMode::Fill), ScaleMode::parse)?;
    let allow_partial = optional_bool(object, "allow_partial")?.unwrap_or(false);
    let preview_paths = match object.get("preview_paths") {
        Some(JsonValue::Array(values)) => values
            .iter()
            .map(|value| value.as_string().map(ToOwned::to_owned))
            .collect::<Result<Vec<_>, _>>()?,
        Some(_) => return Err(ProtocolError::new("field 'preview_paths' must be an array")),
        None => return Err(ProtocolError::new("missing required field 'preview_paths'")),
    };
    let preview_start = optional_usize(object, "preview_start")?.unwrap_or(0);

    Ok(RenderCanvasSetRequest {
        outputs,
        transition,
        scale_mode,
        allow_partial,
        preview_paths,
        preview_start,
    })
}

fn parse_render_world_set_request(
    object: &BTreeMap<String, JsonValue>,
) -> Result<RenderWorldSetRequest, ProtocolError> {
    let outputs = required_outputs(object)?;
    let transition = object
        .get("transition")
        .ok_or_else(|| ProtocolError::new("missing required field 'transition'"))
        .and_then(parse_transition)?;
    let scale_mode =
        optional_string(object, "scale_mode")?.map_or(Ok(ScaleMode::Fill), ScaleMode::parse)?;
    let allow_partial = optional_bool(object, "allow_partial")?.unwrap_or(false);
    let library_count = optional_usize(object, "library_count")?
        .ok_or_else(|| ProtocolError::new("missing required field 'library_count'"))?;
    let columns = optional_usize(object, "columns")?
        .ok_or_else(|| ProtocolError::new("missing required field 'columns'"))?;
    let fingerprint = optional_u64(object, "fingerprint")?
        .ok_or_else(|| ProtocolError::new("missing required field 'fingerprint'"))?;
    let thumbnail_edge = optional_u32(object, "thumbnail_edge")?
        .ok_or_else(|| ProtocolError::new("missing required field 'thumbnail_edge'"))?;
    let tile_cells = optional_usize(object, "tile_cells")?
        .ok_or_else(|| ProtocolError::new("missing required field 'tile_cells'"))?;
    let routes = parse_world_route_map(object)?;

    Ok(RenderWorldSetRequest {
        outputs,
        transition,
        scale_mode,
        allow_partial,
        library_count,
        columns,
        fingerprint,
        thumbnail_edge,
        tile_cells,
        routes,
    })
}

fn parse_world_route_map(
    object: &BTreeMap<String, JsonValue>,
) -> Result<BTreeMap<String, WorldRouteFocus>, ProtocolError> {
    let routes = object
        .get("routes")
        .ok_or_else(|| ProtocolError::new("missing required field 'routes'"))?;
    let routes = routes.as_object()?;
    if routes.is_empty() {
        return Err(ProtocolError::new("field 'routes' must not be empty"));
    }

    routes
        .iter()
        .map(|(name, value)| {
            if name.is_empty() {
                return Err(ProtocolError::new("output name must not be empty"));
            }
            let focus = value.as_object()?;
            let current_index = optional_usize(focus, "current_index")?
                .ok_or_else(|| ProtocolError::new("missing required field 'current_index'"))?;
            let target_index = optional_usize(focus, "target_index")?
                .ok_or_else(|| ProtocolError::new("missing required field 'target_index'"))?;
            let lod = optional_usize(focus, "lod")?.unwrap_or(0);
            Ok((
                name.clone(),
                WorldRouteFocus {
                    current_index,
                    target_index,
                    lod,
                },
            ))
        })
        .collect()
}

fn parse_wallpaper_action(value: &JsonValue) -> Result<WallpaperAction, ProtocolError> {
    if let Ok(action) = value.as_string() {
        return WallpaperAction::parse(action, None);
    }

    let object = value.as_object()?;
    let action = required_string(object, "type")?;
    let index = optional_usize(object, "index")?;
    WallpaperAction::parse(action, index)
}

fn parse_wallpaper_entry(value: &JsonValue) -> Result<WallpaperEntry, ProtocolError> {
    let object = value.as_object()?;
    Ok(WallpaperEntry {
        index: optional_usize(object, "index")?
            .ok_or_else(|| ProtocolError::new("missing required field 'index'"))?,
        output: required_string(object, "output")?.to_owned(),
        favorite: optional_bool(object, "favorite")?.unwrap_or(false),
        path: required_string(object, "path")?.to_owned(),
    })
}

fn parse_health_output(value: &JsonValue) -> Result<HealthOutput, ProtocolError> {
    let object = value.as_object()?;
    Ok(HealthOutput {
        name: required_string(object, "name")?.to_owned(),
        layout_x: optional_i32(object, "layout_x")?.unwrap_or(0),
        layout_y: optional_i32(object, "layout_y")?.unwrap_or(0),
        width: optional_i32(object, "width")?.unwrap_or(0),
        height: optional_i32(object, "height")?.unwrap_or(0),
        power_state: optional_string(object, "power_state")?
            .unwrap_or("unknown")
            .to_owned(),
        render_state: optional_string(object, "render_state")?
            .unwrap_or("unknown")
            .to_owned(),
        restore_pending: optional_bool(object, "restore_pending")?.unwrap_or(false),
        current_image: optional_nullable_string(object, "current_image")?,
        transition_target_image: optional_nullable_string(object, "transition_target_image")?,
        scale_mode: optional_string(object, "scale_mode")?
            .map_or(Ok(ScaleMode::Fill), ScaleMode::parse)?,
        transition_state: parse_transition_state_field(object)?,
        queue_depth: optional_usize(object, "queue_depth")?.unwrap_or(0),
        frame_callback_pending: optional_bool(object, "frame_callback_pending")?.unwrap_or(false),
        render_pending: optional_bool(object, "render_pending")?.unwrap_or(false),
    })
}

fn parse_transition_state_field(
    object: &BTreeMap<String, JsonValue>,
) -> Result<TransitionState, ProtocolError> {
    let Some(value) = object.get("transition_state") else {
        return Ok(TransitionState::Idle);
    };
    let object = value.as_object()?;
    match required_string(object, "state")? {
        "idle" => Ok(TransitionState::Idle),
        "running" => {
            let transition = object
                .get("transition")
                .ok_or_else(|| ProtocolError::new("missing required field 'transition'"))
                .and_then(parse_transition)?;
            Ok(TransitionState::Running { transition })
        }
        state => Err(ProtocolError::new(format!(
            "unknown transition state: {state}"
        ))),
    }
}

fn required_outputs(
    object: &BTreeMap<String, JsonValue>,
) -> Result<BTreeMap<String, String>, ProtocolError> {
    let outputs = object
        .get("outputs")
        .ok_or_else(|| ProtocolError::new("missing required field 'outputs'"))?;

    let outputs_object = outputs.as_object()?;
    if outputs_object.is_empty() {
        return Err(ProtocolError::new("field 'outputs' must not be empty"));
    }

    outputs_object
        .iter()
        .map(|(name, value)| {
            let path = value.as_string()?;
            if name.is_empty() {
                return Err(ProtocolError::new("output name must not be empty"));
            }
            if path.is_empty() {
                return Err(ProtocolError::new(format!(
                    "image path for output {name} must not be empty"
                )));
            }
            Ok((name.clone(), path.to_owned()))
        })
        .collect()
}

fn parse_transition(value: &JsonValue) -> Result<Transition, ProtocolError> {
    if let Ok(token) = value.as_string() {
        return Transition::parse_cli_token(token, DEFAULT_DURATION_MS, Easing::EaseOutCubic);
    }

    let object = value.as_object()?;
    let transition_type = required_string(object, "type")?;
    let kind = transition_descriptor(transition_type)
        .map(|descriptor| descriptor.kind)
        .ok_or_else(|| ProtocolError::new(format!("unknown transition type: {transition_type}")))?;
    let duration_ms = optional_u64(object, "duration_ms")?.unwrap_or(DEFAULT_DURATION_MS);
    let easing =
        optional_string(object, "easing")?.map_or(Ok(Easing::EaseOutCubic), Easing::parse)?;

    match kind {
        TransitionKind::Cut => Ok(Transition::Cut),
        TransitionKind::Fade => {
            validate_positive_milliseconds(duration_ms, "duration_ms")?;
            Ok(Transition::Fade {
                duration_ms,
                easing,
            })
        }
        TransitionKind::World => {
            validate_positive_milliseconds(duration_ms, "duration_ms")?;
            Ok(Transition::World {
                duration_ms,
                easing,
            })
        }
        TransitionKind::Push => {
            validate_positive_milliseconds(duration_ms, "duration_ms")?;
            let direction = PushDirection::parse(required_string(object, "direction")?)?;
            let mode =
                optional_string(object, "mode")?.map_or(Ok(PushMode::Portal), PushMode::parse)?;
            Ok(Transition::Push {
                direction,
                duration_ms,
                easing,
                mode,
            })
        }
        TransitionKind::Canvas => {
            let zoom_out_ms = optional_u64(object, "zoom_out_ms")?.unwrap_or(DEFAULT_CANVAS_OUT_MS);
            let pan_ms = optional_u64(object, "pan_ms")?.unwrap_or(DEFAULT_CANVAS_PAN_MS);
            let zoom_in_ms = optional_u64(object, "zoom_in_ms")?.unwrap_or(DEFAULT_CANVAS_IN_MS);
            validate_positive_milliseconds(zoom_out_ms, "zoom_out_ms")?;
            validate_positive_milliseconds(pan_ms, "pan_ms")?;
            validate_positive_milliseconds(zoom_in_ms, "zoom_in_ms")?;
            let mode = optional_string(object, "mode")?
                .map_or(Ok(CanvasMode::Clipped), CanvasMode::parse)?;
            let walk = optional_string(object, "walk")?
                .map_or(Ok(CanvasWalk::Paged), CanvasWalk::parse)?;
            let pan_axis = optional_string(object, "pan_axis")?
                .map_or(Ok(CanvasPanAxis::Auto), CanvasPanAxis::parse)?;
            let overview_scale =
                optional_f32(object, "overview_scale")?.unwrap_or(DEFAULT_CANVAS_OVERVIEW_SCALE);
            validate_canvas_overview_scale(overview_scale)?;
            validate_canvas_mode_walk(mode, walk)?;
            let tile_count = parse_canvas_tile_count_policy(object)?;
            Ok(Transition::Canvas {
                zoom_out_ms,
                pan_ms,
                zoom_in_ms,
                easing,
                mode,
                walk,
                pan_axis,
                overview_scale,
                tile_count,
            })
        }
    }
}

fn required_string<'a>(
    object: &'a BTreeMap<String, JsonValue>,
    key: &str,
) -> Result<&'a str, ProtocolError> {
    object
        .get(key)
        .ok_or_else(|| ProtocolError::new(format!("missing required field '{key}'")))?
        .as_string()
}

fn optional_string<'a>(
    object: &'a BTreeMap<String, JsonValue>,
    key: &str,
) -> Result<Option<&'a str>, ProtocolError> {
    object.get(key).map(JsonValue::as_string).transpose()
}

fn optional_bool(
    object: &BTreeMap<String, JsonValue>,
    key: &str,
) -> Result<Option<bool>, ProtocolError> {
    object.get(key).map(JsonValue::as_bool).transpose()
}

fn required_bool(object: &BTreeMap<String, JsonValue>, key: &str) -> Result<bool, ProtocolError> {
    object
        .get(key)
        .ok_or_else(|| ProtocolError::new(format!("missing required field '{key}'")))?
        .as_bool()
}

fn optional_nullable_string(
    object: &BTreeMap<String, JsonValue>,
    key: &str,
) -> Result<Option<String>, ProtocolError> {
    match object.get(key) {
        Some(JsonValue::Null) | None => Ok(None),
        Some(value) => value.as_string().map(ToOwned::to_owned).map(Some),
    }
}

fn optional_u64(
    object: &BTreeMap<String, JsonValue>,
    key: &str,
) -> Result<Option<u64>, ProtocolError> {
    match object.get(key) {
        Some(JsonValue::Number(number)) => {
            if number.starts_with('-') {
                return Err(ProtocolError::new(format!(
                    "field '{key}' must not be negative"
                )));
            }
            if number.contains(['.', 'e', 'E']) {
                return Err(ProtocolError::new(format!(
                    "field '{key}' must be an integer"
                )));
            }
            number
                .parse::<u64>()
                .map(Some)
                .map_err(|_| ProtocolError::new(format!("field '{key}' is out of range")))
        }
        Some(_) => Err(ProtocolError::new(format!(
            "field '{key}' must be an integer"
        ))),
        None => Ok(None),
    }
}

fn optional_u32(
    object: &BTreeMap<String, JsonValue>,
    key: &str,
) -> Result<Option<u32>, ProtocolError> {
    match object.get(key) {
        Some(JsonValue::Null) | None => Ok(None),
        Some(JsonValue::Number(number)) => parse_json_u32(number, key).map(Some),
        Some(_) => Err(ProtocolError::new(format!(
            "field '{key}' must be an integer"
        ))),
    }
}

fn optional_i32(
    object: &BTreeMap<String, JsonValue>,
    key: &str,
) -> Result<Option<i32>, ProtocolError> {
    match object.get(key) {
        Some(JsonValue::Number(number)) => parse_json_i32(number, key).map(Some),
        Some(_) => Err(ProtocolError::new(format!(
            "field '{key}' must be an integer"
        ))),
        None => Ok(None),
    }
}

fn optional_usize(
    object: &BTreeMap<String, JsonValue>,
    key: &str,
) -> Result<Option<usize>, ProtocolError> {
    match object.get(key) {
        Some(JsonValue::Number(number)) => parse_json_usize(number, key).map(Some),
        Some(_) => Err(ProtocolError::new(format!(
            "field '{key}' must be an integer"
        ))),
        None => Ok(None),
    }
}

fn optional_f32(
    object: &BTreeMap<String, JsonValue>,
    key: &str,
) -> Result<Option<f32>, ProtocolError> {
    match object.get(key) {
        Some(JsonValue::Number(number)) => parse_json_f32(number, key).map(Some),
        Some(JsonValue::String(value)) => parse_json_f32(value, key).map(Some),
        Some(_) => Err(ProtocolError::new(format!(
            "field '{key}' must be a number"
        ))),
        None => Ok(None),
    }
}

fn required_index(action: &str, index: Option<usize>) -> Result<usize, ProtocolError> {
    index.ok_or_else(|| ProtocolError::new(format!("{action} requires field 'index'")))
}

fn parse_canvas_tile_count_policy(
    object: &BTreeMap<String, JsonValue>,
) -> Result<CanvasTileCount, ProtocolError> {
    let max = optional_usize(object, "max_tile_count")?;
    if let Some(max) = max {
        validate_canvas_tile_count(max, "canvas max_tile_count")?;
    }

    match object.get("tile_count") {
        Some(JsonValue::String(value)) if value == "auto" => Ok(CanvasTileCount::Auto { max }),
        Some(JsonValue::String(value)) => Err(ProtocolError::new(format!(
            "field 'tile_count' must be auto or an integer, got {value}"
        ))),
        Some(JsonValue::Number(number)) => {
            let tile_count = parse_json_usize(number, "tile_count")?;
            validate_canvas_tile_count(tile_count, "canvas tile_count")?;
            Ok(CanvasTileCount::Fixed(tile_count))
        }
        Some(_) => Err(ProtocolError::new(
            "field 'tile_count' must be auto or an integer",
        )),
        None => Ok(CanvasTileCount::Auto { max }),
    }
}

fn parse_json_u32(number: &str, key: &str) -> Result<u32, ProtocolError> {
    if number.starts_with('-') {
        return Err(ProtocolError::new(format!(
            "field '{key}' must not be negative"
        )));
    }
    if number.contains(['.', 'e', 'E']) {
        return Err(ProtocolError::new(format!(
            "field '{key}' must be an integer"
        )));
    }
    number
        .parse::<u32>()
        .map_err(|_| ProtocolError::new(format!("field '{key}' is out of range")))
}

fn parse_json_i32(number: &str, key: &str) -> Result<i32, ProtocolError> {
    if number.contains(['.', 'e', 'E']) {
        return Err(ProtocolError::new(format!(
            "field '{key}' must be an integer"
        )));
    }
    number
        .parse::<i32>()
        .map_err(|_| ProtocolError::new(format!("field '{key}' is out of range")))
}

fn parse_json_usize(number: &str, key: &str) -> Result<usize, ProtocolError> {
    if number.starts_with('-') {
        return Err(ProtocolError::new(format!(
            "field '{key}' must not be negative"
        )));
    }
    if number.contains(['.', 'e', 'E']) {
        return Err(ProtocolError::new(format!(
            "field '{key}' must be an integer"
        )));
    }
    number
        .parse::<usize>()
        .map_err(|_| ProtocolError::new(format!("field '{key}' is out of range")))
}

fn parse_json_f32(number: &str, key: &str) -> Result<f32, ProtocolError> {
    number
        .parse::<f32>()
        .map_err(|_| ProtocolError::new(format!("field '{key}' must be a number")))
}

fn validate_canvas_overview_scale(scale: f32) -> Result<(), ProtocolError> {
    if !scale.is_finite() || scale <= 0.0 || scale > 1.0 {
        return Err(ProtocolError::new(
            "canvas overview_scale must be greater than 0 and at most 1",
        ));
    }
    Ok(())
}

fn validate_positive_milliseconds(value: u64, field: &str) -> Result<(), ProtocolError> {
    if value == 0 {
        return Err(ProtocolError::new(format!(
            "field '{field}' must be greater than zero"
        )));
    }
    Ok(())
}

fn validate_canvas_tile_count(tile_count: usize, field: &str) -> Result<(), ProtocolError> {
    if tile_count == 0 {
        return Err(ProtocolError::new(format!(
            "{field} must be greater than zero"
        )));
    }
    if tile_count > MAX_CANVAS_TILE_COUNT {
        return Err(ProtocolError::new(format!(
            "{field} must be at most {MAX_CANVAS_TILE_COUNT}"
        )));
    }
    Ok(())
}

fn validate_cache_workers(workers: usize) -> Result<(), ProtocolError> {
    if workers == 0 {
        return Err(ProtocolError::new(
            "cache workers must be greater than zero",
        ));
    }
    Ok(())
}

fn encode_optional_u32(value: Option<u32>) -> String {
    value.map_or_else(|| "null".to_owned(), |value| value.to_string())
}

fn encode_transition_capabilities(transitions: &[TransitionCapability]) -> String {
    let mut encoded = String::from("[");
    for (index, transition) in transitions.iter().enumerate() {
        if index > 0 {
            encoded.push(',');
        }
        encoded.push_str(&object([
            ("name", json_string(&transition.name)),
            ("class", json_string(transition.class.as_str())),
            (
                "scopes",
                object([
                    ("explicit_set", transition.scopes.explicit_set.to_string()),
                    (
                        "wallpaper_actions",
                        transition.scopes.wallpaper_actions.to_string(),
                    ),
                ]),
            ),
            ("experimental", transition.experimental.to_string()),
            (
                "requirements",
                encode_string_array(&transition.requirements),
            ),
            (
                "parameters",
                encode_transition_parameter_capabilities(&transition.parameters),
            ),
        ]));
    }
    encoded.push(']');
    encoded
}

fn encode_transition_parameter_capabilities(
    parameters: &[TransitionParameterCapability],
) -> String {
    let mut encoded = String::from("[");
    for (index, parameter) in parameters.iter().enumerate() {
        if index > 0 {
            encoded.push(',');
        }
        encoded.push_str(&object([
            ("name", json_string(&parameter.name)),
            ("type", json_string(parameter.value_type.as_str())),
            (
                "allowed_values",
                encode_string_array(&parameter.allowed_values),
            ),
            ("required", parameter.required.to_string()),
            (
                "default",
                parameter
                    .default_value
                    .as_ref()
                    .map_or_else(|| "null".to_owned(), encode_transition_parameter_value),
            ),
            (
                "constraint",
                parameter
                    .constraint
                    .as_deref()
                    .map_or_else(|| "null".to_owned(), json_string),
            ),
        ]));
    }
    encoded.push(']');
    encoded
}

fn encode_transition_parameter_value(value: &TransitionParameterValue) -> String {
    match value {
        TransitionParameterValue::Integer(value) => value.to_string(),
        TransitionParameterValue::Number(value) => value.to_string(),
        TransitionParameterValue::String(value) => json_string(value),
    }
}

fn encode_health_outputs(outputs: &[HealthOutput]) -> String {
    let mut encoded = String::from("[");
    for (index, output) in outputs.iter().enumerate() {
        if index > 0 {
            encoded.push(',');
        }
        encoded.push_str(&object([
            ("name", json_string(&output.name)),
            ("layout_x", output.layout_x.to_string()),
            ("layout_y", output.layout_y.to_string()),
            ("width", output.width.to_string()),
            ("height", output.height.to_string()),
            ("power_state", json_string(&output.power_state)),
            ("render_state", json_string(&output.render_state)),
            ("restore_pending", output.restore_pending.to_string()),
            (
                "current_image",
                output
                    .current_image
                    .as_deref()
                    .map_or_else(|| "null".to_owned(), json_string),
            ),
            (
                "transition_target_image",
                output
                    .transition_target_image
                    .as_deref()
                    .map_or_else(|| "null".to_owned(), json_string),
            ),
            ("scale_mode", json_string(output.scale_mode.as_str())),
            (
                "transition_state",
                encode_transition_state(&output.transition_state),
            ),
            ("queue_depth", output.queue_depth.to_string()),
            (
                "frame_callback_pending",
                output.frame_callback_pending.to_string(),
            ),
            ("render_pending", output.render_pending.to_string()),
        ]));
    }
    encoded.push(']');
    encoded
}

fn encode_wallpaper_action(action: &WallpaperAction) -> String {
    match action {
        WallpaperAction::Replace { index }
        | WallpaperAction::Quarantine { index }
        | WallpaperAction::Favorite { index }
        | WallpaperAction::Unfavorite { index } => object([
            ("type", json_string(action.as_str())),
            ("index", index.to_string()),
        ]),
        _ => json_string(action.as_str()),
    }
}

fn encode_transition(transition: Transition) -> String {
    match transition {
        Transition::Cut => object([("type", json_string("cut"))]),
        Transition::Fade {
            duration_ms,
            easing,
        } => object([
            ("type", json_string("fade")),
            ("duration_ms", duration_ms.to_string()),
            ("easing", json_string(easing.as_str())),
        ]),
        Transition::World {
            duration_ms,
            easing,
        } => object([
            ("type", json_string("world")),
            ("duration_ms", duration_ms.to_string()),
            ("easing", json_string(easing.as_str())),
        ]),
        Transition::Push {
            direction,
            duration_ms,
            easing,
            mode,
        } => object([
            ("type", json_string("push")),
            ("direction", json_string(direction.as_str())),
            ("duration_ms", duration_ms.to_string()),
            ("easing", json_string(easing.as_str())),
            ("mode", json_string(mode.as_str())),
        ]),
        Transition::Canvas {
            zoom_out_ms,
            pan_ms,
            zoom_in_ms,
            easing,
            mode,
            walk,
            pan_axis,
            overview_scale,
            tile_count,
        } => {
            let mut fields = vec![
                ("type", json_string("canvas")),
                ("zoom_out_ms", zoom_out_ms.to_string()),
                ("pan_ms", pan_ms.to_string()),
                ("zoom_in_ms", zoom_in_ms.to_string()),
                ("easing", json_string(easing.as_str())),
                ("mode", json_string(mode.as_str())),
                ("walk", json_string(walk.as_str())),
                ("pan_axis", json_string(pan_axis.as_str())),
                ("overview_scale", format_overview_scale(overview_scale)),
            ];
            match tile_count {
                CanvasTileCount::Auto { max } => {
                    fields.push(("tile_count", json_string("auto")));
                    if let Some(max) = max {
                        fields.push(("max_tile_count", max.to_string()));
                    }
                }
                CanvasTileCount::Fixed(tile_count) => {
                    fields.push(("tile_count", tile_count.to_string()));
                }
            }
            object(fields)
        }
    }
}

fn format_overview_scale(scale: f32) -> String {
    let mut formatted = format!("{scale:.6}");
    while formatted.contains('.') && formatted.ends_with('0') {
        formatted.pop();
    }
    if formatted.ends_with('.') {
        formatted.push('0');
    }
    formatted
}

fn encode_outputs(outputs: &[OutputState]) -> String {
    let mut encoded = String::from("[");
    for (index, output) in outputs.iter().enumerate() {
        if index > 0 {
            encoded.push(',');
        }
        encoded.push_str(&object([
            ("name", json_string(&output.name)),
            (
                "current_image",
                output
                    .current_image
                    .as_deref()
                    .map_or_else(|| "null".to_owned(), json_string),
            ),
            ("scale_mode", json_string(output.scale_mode.as_str())),
            (
                "transition_state",
                encode_transition_state(&output.transition_state),
            ),
            ("queue_depth", output.queue_depth.to_string()),
        ]));
    }
    encoded.push(']');
    encoded
}

fn encode_transition_state(state: &TransitionState) -> String {
    match state {
        TransitionState::Idle => object([("state", json_string("idle"))]),
        TransitionState::Running { transition } => object([
            ("state", json_string("running")),
            ("transition", encode_transition(*transition)),
        ]),
    }
}

fn encode_wallpaper_entries(entries: &[WallpaperEntry]) -> String {
    let mut encoded = String::from("[");
    for (index, entry) in entries.iter().enumerate() {
        if index > 0 {
            encoded.push(',');
        }
        encoded.push_str(&object([
            ("index", entry.index.to_string()),
            ("output", json_string(&entry.output)),
            ("favorite", entry.favorite.to_string()),
            ("path", json_string(&entry.path)),
        ]));
    }
    encoded.push(']');
    encoded
}

fn encode_string_map(map: &BTreeMap<String, String>) -> String {
    let mut encoded = String::from("{");
    for (index, (key, value)) in map.iter().enumerate() {
        if index > 0 {
            encoded.push(',');
        }
        encoded.push_str(&json_string(key));
        encoded.push(':');
        encoded.push_str(&json_string(value));
    }
    encoded.push('}');
    encoded
}

fn encode_world_route_map(map: &BTreeMap<String, WorldRouteFocus>) -> String {
    let mut encoded = String::from("{");
    for (index, (key, value)) in map.iter().enumerate() {
        if index > 0 {
            encoded.push(',');
        }
        encoded.push_str(&json_string(key));
        encoded.push(':');
        encoded.push_str(&object([
            ("current_index", value.current_index.to_string()),
            ("target_index", value.target_index.to_string()),
            ("lod", value.lod.to_string()),
        ]));
    }
    encoded.push('}');
    encoded
}

fn encode_string_array(values: &[String]) -> String {
    let mut encoded = String::from("[");
    for (index, value) in values.iter().enumerate() {
        if index > 0 {
            encoded.push(',');
        }
        encoded.push_str(&json_string(value));
    }
    encoded.push(']');
    encoded
}

fn object(fields: impl IntoIterator<Item = (&'static str, String)>) -> String {
    let mut encoded = String::from("{");
    for (index, (key, value)) in fields.into_iter().enumerate() {
        if index > 0 {
            encoded.push(',');
        }
        encoded.push_str(&json_string(key));
        encoded.push(':');
        encoded.push_str(&value);
    }
    encoded.push('}');
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
            '\u{08}' => encoded.push_str("\\b"),
            '\u{0c}' => encoded.push_str("\\f"),
            character if character.is_control() => {
                let _ = write!(encoded, "\\u{:04x}", u32::from(character));
            }
            character => encoded.push(character),
        }
    }
    encoded.push('"');
    encoded
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_round_trip_for_set_push() {
        let request = Request::Set(SetRequest {
            outputs: BTreeMap::from([("DP-1".to_owned(), "/tmp/wall.jpg".to_owned())]),
            transition: Transition::Push {
                direction: PushDirection::Up,
                duration_ms: 900,
                easing: Easing::EaseOutCubic,
                mode: PushMode::Portal,
            },
            scale_mode: ScaleMode::Fill,
            allow_partial: false,
        });

        assert_eq!(parse_request(&request.to_json()).unwrap(), request);
    }

    #[test]
    fn documented_set_request_shape_stays_compatible() {
        let request = parse_request(
            r#"{
              "type": "set",
              "outputs": {
                "DP-1": "/home/user/a.jpg",
                "DP-2": "/home/user/b.jpg"
              },
              "transition": {
                "type": "push",
                "direction": "up",
                "duration_ms": 900,
                "easing": "ease-out-cubic",
                "mode": "portal"
              },
              "scale_mode": "fill",
              "allow_partial": false
            }"#,
        )
        .unwrap();

        assert_eq!(
            request,
            Request::Set(SetRequest {
                outputs: BTreeMap::from([
                    ("DP-1".to_owned(), "/home/user/a.jpg".to_owned()),
                    ("DP-2".to_owned(), "/home/user/b.jpg".to_owned()),
                ]),
                transition: Transition::Push {
                    direction: PushDirection::Up,
                    duration_ms: 900,
                    easing: Easing::EaseOutCubic,
                    mode: PushMode::Portal,
                },
                scale_mode: ScaleMode::Fill,
                allow_partial: false,
            })
        );
    }

    #[test]
    fn documented_canvas_transition_shape_stays_compatible() {
        let request = parse_request(
            r#"{
              "type": "set",
              "outputs": {"DP-1": "/home/user/a.jpg"},
              "transition": {
                "type": "canvas",
                "zoom_out_ms": 180,
                "pan_ms": 80,
                "zoom_in_ms": 260,
                "easing": "ease-out-cubic",
                "mode": "clipped",
                "walk": "paged",
                "pan_axis": "auto",
                "overview_scale": 0.333333,
                "tile_count": "auto",
                "max_tile_count": 48
              }
            }"#,
        )
        .unwrap();

        assert_eq!(
            request,
            Request::Set(SetRequest {
                outputs: BTreeMap::from([("DP-1".to_owned(), "/home/user/a.jpg".to_owned())]),
                transition: Transition::Canvas {
                    zoom_out_ms: 180,
                    pan_ms: 80,
                    zoom_in_ms: 260,
                    easing: Easing::EaseOutCubic,
                    mode: CanvasMode::Clipped,
                    walk: CanvasWalk::Paged,
                    pan_axis: CanvasPanAxis::Auto,
                    overview_scale: 0.333_333,
                    tile_count: CanvasTileCount::Auto { max: Some(48) },
                },
                scale_mode: ScaleMode::Fill,
                allow_partial: false,
            })
        );
    }

    #[test]
    fn animated_transition_timings_must_be_positive() {
        for (transition, field) in [
            (r#"{"type":"fade","duration_ms":0}"#, "duration_ms"),
            (
                r#"{"type":"push","direction":"left","duration_ms":0}"#,
                "duration_ms",
            ),
            (r#"{"type":"world","duration_ms":0}"#, "duration_ms"),
            (r#"{"type":"canvas","zoom_out_ms":0}"#, "zoom_out_ms"),
            (r#"{"type":"canvas","pan_ms":0}"#, "pan_ms"),
            (r#"{"type":"canvas","zoom_in_ms":0}"#, "zoom_in_ms"),
        ] {
            let error = parse_request(&format!(
                r#"{{"type":"set","outputs":{{"DP-1":"/tmp/a.jpg"}},"transition":{transition}}}"#
            ))
            .unwrap_err();

            assert_eq!(
                error.message(),
                format!("field '{field}' must be greater than zero")
            );
        }

        assert_eq!(
            Transition::parse_cli_token("fade", 0, Easing::Linear)
                .unwrap_err()
                .message(),
            "field 'duration_ms' must be greater than zero"
        );
    }

    #[test]
    fn compact_transition_token_is_accepted() {
        let request = parse_request(
            r#"{
                "type":"set",
                "outputs":{"DP-1":"/tmp/a.jpg"},
                "transition":"push:left",
                "scale_mode":"fit"
            }"#,
        )
        .unwrap();

        assert_eq!(
            request,
            Request::Set(SetRequest {
                outputs: BTreeMap::from([("DP-1".to_owned(), "/tmp/a.jpg".to_owned())]),
                transition: Transition::Push {
                    direction: PushDirection::Left,
                    duration_ms: DEFAULT_DURATION_MS,
                    easing: Easing::EaseOutCubic,
                    mode: PushMode::Portal,
                },
                scale_mode: ScaleMode::Fit,
                allow_partial: false,
            })
        );
    }

    #[test]
    fn compact_world_transition_token_is_accepted() {
        let request = parse_request(
            r#"{
                "type":"wallpaper",
                "action":"next",
                "transition":"world"
            }"#,
        )
        .unwrap();

        assert_eq!(
            request,
            Request::Wallpaper(WallpaperRequest {
                action: WallpaperAction::Next,
                transition: Some(Transition::World {
                    duration_ms: DEFAULT_DURATION_MS,
                    easing: Easing::EaseOutCubic,
                }),
                scale_mode: None,
            })
        );
    }

    #[test]
    fn world_transition_round_trips() {
        let request = Request::Wallpaper(WallpaperRequest {
            action: WallpaperAction::Next,
            transition: Some(Transition::World {
                duration_ms: 1400,
                easing: Easing::EaseInOutCubic,
            }),
            scale_mode: Some(ScaleMode::Fill),
        });

        assert_eq!(parse_request(&request.to_json()).unwrap(), request);
    }

    #[test]
    fn explicit_push_mode_is_parsed() {
        let request = parse_request(
            r#"{
                "type":"set",
                "outputs":{"DP-1":"/tmp/a.jpg"},
                "transition":{
                    "type":"push",
                    "direction":"right",
                    "mode":"screen"
                }
            }"#,
        )
        .unwrap();

        assert_eq!(
            request,
            Request::Set(SetRequest {
                outputs: BTreeMap::from([("DP-1".to_owned(), "/tmp/a.jpg".to_owned())]),
                transition: Transition::Push {
                    direction: PushDirection::Right,
                    duration_ms: DEFAULT_DURATION_MS,
                    easing: Easing::EaseOutCubic,
                    mode: PushMode::Screen,
                },
                scale_mode: ScaleMode::Fill,
                allow_partial: false,
            })
        );
    }

    #[test]
    fn canvas_transition_round_trips() {
        let request = Request::Wallpaper(WallpaperRequest {
            action: WallpaperAction::Next,
            transition: Some(Transition::Canvas {
                zoom_out_ms: 120,
                pan_ms: 40,
                zoom_in_ms: 180,
                easing: Easing::EaseInOutCubic,
                mode: CanvasMode::Overlap,
                walk: CanvasWalk::Strip,
                pan_axis: CanvasPanAxis::Horizontal,
                overview_scale: 0.25,
                tile_count: CanvasTileCount::Fixed(9),
            }),
            scale_mode: Some(ScaleMode::Fill),
        });

        assert_eq!(parse_request(&request.to_json()).unwrap(), request);
    }

    #[test]
    fn canvas_modes_parse_and_format() {
        for mode in [
            CanvasMode::Clipped,
            CanvasMode::Morph,
            CanvasMode::Overlap,
            CanvasMode::Collage,
            CanvasMode::Span,
        ] {
            assert_eq!(CanvasMode::parse(mode.as_str()).unwrap(), mode);
        }
    }

    #[test]
    fn legacy_canvas_mode_aliases_are_rejected() {
        for mode in ["screen", "puzzle", "spiral", "reveal", "aspect"] {
            assert!(CanvasMode::parse(mode).is_err());
        }
    }

    #[test]
    fn canvas_collage_rejects_paged_walk() {
        for mode in ["collage", "span"] {
            let error = parse_request(&format!(
                r#"{{
                    "type":"wallpaper",
                    "action":"next",
                    "transition":{{
                        "type":"canvas",
                        "mode":"{mode}",
                        "walk":"paged"
                    }}
                }}"#,
            ))
            .unwrap_err();

            assert!(error.to_string().contains("requires canvas walk 'strip'"));
        }
    }

    #[test]
    fn canvas_walks_parse_and_format() {
        for walk in [CanvasWalk::Paged, CanvasWalk::Strip] {
            assert_eq!(CanvasWalk::parse(walk.as_str()).unwrap(), walk);
        }
    }

    #[test]
    fn compact_canvas_token_is_accepted() {
        let request = parse_request(
            r#"{
                "type":"wallpaper",
                "action":"next",
                "transition":"canvas"
            }"#,
        )
        .unwrap();

        assert_eq!(
            request,
            Request::Wallpaper(WallpaperRequest {
                action: WallpaperAction::Next,
                transition: Some(Transition::Canvas {
                    zoom_out_ms: DEFAULT_CANVAS_OUT_MS,
                    pan_ms: DEFAULT_CANVAS_PAN_MS,
                    zoom_in_ms: DEFAULT_CANVAS_IN_MS,
                    easing: Easing::EaseOutCubic,
                    mode: CanvasMode::Clipped,
                    walk: CanvasWalk::Paged,
                    pan_axis: CanvasPanAxis::Auto,
                    overview_scale: DEFAULT_CANVAS_OVERVIEW_SCALE,
                    tile_count: CanvasTileCount::Auto { max: None },
                }),
                scale_mode: None,
            })
        );
    }

    #[test]
    fn cache_warm_request_round_trips() {
        let request = Request::Cache(CacheRequest {
            action: CacheAction::Warm {
                scope: CacheWarmScope::All,
                workers: 8,
                backend: CacheBackend::Vips,
            },
        });

        assert_eq!(parse_request(&request.to_json()).unwrap(), request);
    }

    #[test]
    fn cache_clear_request_round_trips() {
        let request = Request::Cache(CacheRequest {
            action: CacheAction::Clear,
        });

        assert_eq!(parse_request(&request.to_json()).unwrap(), request);
    }

    #[test]
    fn health_request_round_trips() {
        assert_eq!(
            parse_request(&Request::Health.to_json()).unwrap(),
            Request::Health
        );
    }

    #[test]
    fn capabilities_request_round_trips() {
        assert_eq!(
            parse_request(&Request::Capabilities.to_json()).unwrap(),
            Request::Capabilities
        );
    }

    #[test]
    fn compiled_in_transition_registry_is_complete_and_stable() {
        assert_eq!(
            transition_registry()
                .iter()
                .map(|descriptor| descriptor.name)
                .collect::<Vec<_>>(),
            ["cut", "fade", "push", "canvas", "world"]
        );

        let fade = transition_descriptor("fade").unwrap();
        assert_eq!(fade.class, TransitionClass::Pairwise);
        assert!(fade.scopes.explicit_set);
        assert!(fade.scopes.wallpaper_actions);

        let canvas = transition_descriptor("canvas").unwrap();
        assert_eq!(canvas.class, TransitionClass::Scene);
        assert!(!canvas.scopes.explicit_set);
        assert!(canvas.scopes.wallpaper_actions);
        assert_eq!(canvas.requirements, ["wallpaper action history"]);

        let world = transition_descriptor("world").unwrap();
        assert!(world.experimental);
        assert_eq!(
            world.requirements,
            [
                "supervisor planning",
                "wallpaper library",
                "ready cache coverage"
            ]
        );
    }

    #[test]
    fn capabilities_response_round_trips_registry_schema() {
        let capabilities = CapabilitiesResponse::current(DaemonMode::Supervisor);
        let response_json = Response::Capabilities(capabilities.clone()).to_json();

        assert_eq!(
            parse_capabilities_response(&response_json).unwrap(),
            capabilities
        );
        assert!(response_json.contains("\"schema_version\":1"));
        assert!(response_json.contains("\"daemon_mode\":\"supervisor\""));
        assert!(response_json.contains("\"class\":\"pairwise\""));
        assert!(response_json.contains("\"explicit_set\":false"));
        assert!(response_json.contains("\"requirements\":[\"wallpaper action history\"]"));
        assert!(response_json.contains("\"allowed_values\":[\"up\",\"down\",\"left\",\"right\"]"));
        assert!(response_json.contains("\"required\":true,\"default\":null"));
        assert!(response_json.contains("\"default\":900"));
        assert!(response_json.contains("\"default\":0.333333"));
        assert!(!response_json.contains("\"default\":\"900\""));

        let fade = capabilities
            .transitions
            .iter()
            .find(|transition| transition.name == "fade")
            .unwrap();
        assert_eq!(
            fade.parameters[0].default_value,
            Some(TransitionParameterValue::Integer(900))
        );
        let push = capabilities
            .transitions
            .iter()
            .find(|transition| transition.name == "push")
            .unwrap();
        assert!(push.parameters[0].required);
        assert_eq!(push.parameters[0].default_value, None);
        let canvas = capabilities
            .transitions
            .iter()
            .find(|transition| transition.name == "canvas")
            .unwrap();
        assert_eq!(
            canvas
                .parameters
                .iter()
                .find(|parameter| parameter.name == "overview_scale")
                .unwrap()
                .default_value,
            Some(TransitionParameterValue::Number(0.333_333))
        );
        assert_eq!(
            canvas
                .parameters
                .iter()
                .find(|parameter| parameter.name == "tile_count")
                .unwrap()
                .default_value,
            Some(TransitionParameterValue::String("auto".to_owned()))
        );
    }

    #[test]
    fn standalone_capabilities_keep_world_metadata_but_disable_its_scopes() {
        let capabilities = CapabilitiesResponse::current(DaemonMode::Standalone);
        let world = capabilities
            .transitions
            .iter()
            .find(|transition| transition.name == "world")
            .unwrap();

        assert_eq!(capabilities.daemon_mode, DaemonMode::Standalone);
        assert!(!world.scopes.explicit_set);
        assert!(!world.scopes.wallpaper_actions);
        assert_eq!(
            world.requirements,
            [
                "supervisor planning",
                "wallpaper library",
                "ready cache coverage"
            ]
        );
        assert!(!world.parameters.is_empty());
    }

    #[test]
    fn capabilities_parser_rejects_unsupported_schema_versions() {
        for schema_version in [0, 2] {
            let error = parse_capabilities_response(&format!(
                r#"{{"status":"ok","type":"capabilities","schema_version":{schema_version},"protocol_version":1,"daemon_mode":"supervisor","transitions":[]}}"#
            ))
            .unwrap_err();

            assert_eq!(
                error.message(),
                format!(
                    "unsupported capabilities schema version {schema_version}; supported version is {CAPABILITIES_SCHEMA_VERSION}"
                )
            );
        }
    }

    #[test]
    fn number_defaults_accept_integral_json_numbers() {
        let response = parse_capabilities_response(
            r#"{"status":"ok","type":"capabilities","schema_version":1,"protocol_version":1,"daemon_mode":"supervisor","transitions":[{"name":"example","class":"pairwise","scopes":{"explicit_set":true,"wallpaper_actions":false},"experimental":false,"requirements":[],"parameters":[{"name":"ratio","type":"number","allowed_values":[],"required":false,"default":1,"constraint":null}]}]}"#,
        )
        .unwrap();

        assert_eq!(
            response.transitions[0].parameters[0].default_value,
            Some(TransitionParameterValue::Number(1.0))
        );
    }

    #[test]
    fn fallback_socket_path_requires_a_reliable_uid() {
        assert!(fallback_socket_path_for_uid(None).is_err());
        assert_eq!(
            fallback_socket_path_for_uid(Some(1000)).unwrap(),
            PathBuf::from("/tmp/mural-1000/mural.sock")
        );
    }

    #[test]
    fn public_request_parser_accepts_client_requests() {
        for request in [Request::Health, Request::Capabilities] {
            assert_eq!(parse_public_request(&request.to_json()).unwrap(), request);
        }
    }

    #[test]
    fn public_request_parser_rejects_renderer_control_requests() {
        for request_type in ["renderer_canvas_set", "renderer_world_set"] {
            let error = parse_public_request(&format!(r#"{{"type":"{request_type}"}}"#))
                .expect_err("renderer control request should not be public");

            assert_eq!(
                error.message(),
                format!(
                    "request type is reserved for the internal renderer control channel: {request_type}"
                )
            );
        }
    }

    #[test]
    fn health_response_parser_reads_outputs() {
        let response = Response::Health(Box::new(HealthResponse {
            role: "supervisor".to_owned(),
            supervisor_pid: Some(10),
            renderer_pid: Some(11),
            renderer_generation: 2,
            renderer_state: "running".to_owned(),
            restart_count: 1,
            last_error: None,
            last_diagnostic: Some("/tmp/diag.txt".to_owned()),
            outputs: vec![HealthOutput {
                name: "DP-1".to_owned(),
                layout_x: 0,
                layout_y: 0,
                width: 3840,
                height: 2160,
                power_state: "on".to_owned(),
                render_state: "renderable".to_owned(),
                restore_pending: true,
                current_image: Some("/tmp/wall.jpg".to_owned()),
                transition_target_image: Some("/tmp/target.jpg".to_owned()),
                scale_mode: ScaleMode::Fill,
                transition_state: TransitionState::Idle,
                queue_depth: 0,
                frame_callback_pending: false,
                render_pending: false,
            }],
        }))
        .to_json();

        let parsed = parse_health_response(&response).unwrap();
        assert_eq!(parsed.role, "supervisor");
        assert_eq!(parsed.renderer_pid, Some(11));
        assert_eq!(parsed.outputs[0].name, "DP-1");
        assert_eq!(parsed.outputs[0].render_state, "renderable");
        assert!(parsed.outputs[0].restore_pending);
        assert_eq!(
            parsed.outputs[0].transition_target_image.as_deref(),
            Some("/tmp/target.jpg")
        );
    }

    #[test]
    fn renderer_canvas_set_request_round_trips() {
        let request = Request::RenderCanvasSet(RenderCanvasSetRequest {
            outputs: BTreeMap::from([("DP-1".to_owned(), "/tmp/wall.jpg".to_owned())]),
            transition: Transition::Canvas {
                zoom_out_ms: DEFAULT_CANVAS_OUT_MS,
                pan_ms: DEFAULT_CANVAS_PAN_MS,
                zoom_in_ms: DEFAULT_CANVAS_IN_MS,
                easing: Easing::EaseOutCubic,
                mode: CanvasMode::Clipped,
                walk: CanvasWalk::Paged,
                pan_axis: CanvasPanAxis::Auto,
                overview_scale: 0.333_333,
                tile_count: CanvasTileCount::Auto { max: Some(12) },
            },
            scale_mode: ScaleMode::Fill,
            allow_partial: false,
            preview_paths: vec!["/tmp/old.jpg".to_owned(), "/tmp/wall.jpg".to_owned()],
            preview_start: 3,
        });

        assert_eq!(parse_request(&request.to_json()).unwrap(), request);
    }

    #[test]
    fn renderer_world_set_request_round_trips() {
        let request = Request::RenderWorldSet(RenderWorldSetRequest {
            outputs: BTreeMap::from([("DP-1".to_owned(), "/tmp/wall.jpg".to_owned())]),
            transition: Transition::World {
                duration_ms: 1200,
                easing: Easing::EaseInOutCubic,
            },
            scale_mode: ScaleMode::Fill,
            allow_partial: false,
            library_count: 10_000,
            columns: 100,
            fingerprint: 0x1234_5678,
            thumbnail_edge: 384,
            tile_cells: 8,
            routes: BTreeMap::from([(
                "DP-1".to_owned(),
                WorldRouteFocus {
                    current_index: 4,
                    target_index: 444,
                    lod: 1,
                },
            )]),
        });

        assert_eq!(parse_request(&request.to_json()).unwrap(), request);
    }

    #[test]
    fn response_error_detection_reads_json_status() {
        assert!(response_is_error(
            &Response::Error {
                message: "nope".to_owned()
            }
            .to_json()
        ));
        assert!(!response_is_error(&Response::Pong { version: 1 }.to_json()));
    }

    #[test]
    fn json_strings_escape_control_characters() {
        let request = Request::Clear(ClearRequest {
            outputs: vec!["DP-1\nquoted".to_owned()],
            color: "#000000".to_owned(),
        });

        let parsed = parse_request(&request.to_json()).unwrap();
        assert_eq!(parsed, request);
    }

    #[test]
    fn json_parser_combines_unicode_surrogate_pairs() {
        let parsed =
            parse_request(r##"{"type":"clear","outputs":["DP-\ud83d\ude80"],"color":"#000000"}"##)
                .unwrap();

        assert_eq!(
            parsed,
            Request::Clear(ClearRequest {
                outputs: vec!["DP-🚀".to_owned()],
                color: "#000000".to_owned(),
            })
        );
    }

    #[test]
    fn json_parser_rejects_unpaired_unicode_surrogates() {
        for escaped in [r"\ud83dX", r"\ud83d\u0041", r"\ude80"] {
            let input =
                format!(r##"{{"type":"clear","outputs":["{escaped}"],"color":"#000000"}}"##);
            assert!(parse_request(&input).is_err(), "accepted {escaped}");
        }
    }

    #[test]
    fn wallpaper_request_round_trip_with_indexed_action() {
        let request = Request::Wallpaper(WallpaperRequest {
            action: WallpaperAction::Replace { index: 2 },
            transition: Some(Transition::Cut),
            scale_mode: Some(ScaleMode::Fill),
        });

        assert_eq!(parse_request(&request.to_json()).unwrap(), request);
    }

    #[test]
    fn wallpaper_response_parser_reads_entries_and_favorites() {
        let response = Response::Wallpaper(WallpaperResponse {
            action: "current".to_owned(),
            message: String::new(),
            entries: vec![WallpaperEntry {
                index: 0,
                output: "DP-1".to_owned(),
                favorite: true,
                path: "/tmp/wall.jpg".to_owned(),
            }],
            favorites: vec!["/tmp/wall.jpg".to_owned()],
        })
        .to_json();

        assert_eq!(
            parse_wallpaper_response(&response).unwrap(),
            WallpaperResponse {
                action: "current".to_owned(),
                message: String::new(),
                entries: vec![WallpaperEntry {
                    index: 0,
                    output: "DP-1".to_owned(),
                    favorite: true,
                    path: "/tmp/wall.jpg".to_owned(),
                }],
                favorites: vec!["/tmp/wall.jpg".to_owned()],
            }
        );
    }
}
