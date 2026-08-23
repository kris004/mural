//! Compiled-in transition descriptors shared by config, IPC, and CLI code.

/// Stable identifier for a compiled-in transition implementation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransitionKind {
    Cut,
    Fade,
    Push,
    Canvas,
    World,
}

/// Rendering model used by a transition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransitionClass {
    Immediate,
    Pairwise,
    Scene,
}

impl TransitionClass {
    /// Stable capability string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Immediate => "immediate",
            Self::Pairwise => "pairwise",
            Self::Scene => "scene",
        }
    }

    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "immediate" => Some(Self::Immediate),
            "pairwise" => Some(Self::Pairwise),
            "scene" => Some(Self::Scene),
            _ => None,
        }
    }
}

/// Public request scopes supported by a transition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TransitionScopes {
    pub explicit_set: bool,
    pub wallpaper_actions: bool,
}

/// Wire-level value type accepted by a transition parameter.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransitionParameterType {
    Integer,
    Number,
    Enum,
    IntegerOrEnum,
}

impl TransitionParameterType {
    /// Stable capability string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Integer => "integer",
            Self::Number => "number",
            Self::Enum => "enum",
            Self::IntegerOrEnum => "integer-or-enum",
        }
    }

    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "integer" => Some(Self::Integer),
            "number" => Some(Self::Number),
            "enum" => Some(Self::Enum),
            "integer-or-enum" => Some(Self::IntegerOrEnum),
            _ => None,
        }
    }
}

/// Static schema for one transition parameter.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TransitionParameterDescriptor {
    pub name: &'static str,
    pub value_type: TransitionParameterType,
    pub allowed_values: &'static [&'static str],
    pub required: bool,
    pub default_value: Option<TransitionParameterDefault>,
    pub constraint: Option<&'static str>,
}

/// Typed default value compiled into a transition parameter descriptor.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TransitionParameterDefault {
    Integer(i64),
    Number(f64),
    String(&'static str),
}

/// Static schema for one compiled-in transition.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TransitionDescriptor {
    pub kind: TransitionKind,
    pub name: &'static str,
    pub class: TransitionClass,
    pub scopes: TransitionScopes,
    pub experimental: bool,
    pub requirements: &'static [&'static str],
    pub parameters: &'static [TransitionParameterDescriptor],
}

const ALL_PUBLIC_SCOPES: TransitionScopes = TransitionScopes {
    explicit_set: true,
    wallpaper_actions: true,
};

const WALLPAPER_ACTION_SCOPE: TransitionScopes = TransitionScopes {
    explicit_set: false,
    wallpaper_actions: true,
};

const EASING_VALUES: &[&str] = &["linear", "ease-out-cubic", "ease-in-out-cubic"];

const DURATION_PARAMETER: TransitionParameterDescriptor = TransitionParameterDescriptor {
    name: "duration_ms",
    value_type: TransitionParameterType::Integer,
    allowed_values: &[],
    required: false,
    default_value: Some(TransitionParameterDefault::Integer(900)),
    constraint: Some("positive integer milliseconds"),
};

const EASING_PARAMETER: TransitionParameterDescriptor = TransitionParameterDescriptor {
    name: "easing",
    value_type: TransitionParameterType::Enum,
    allowed_values: EASING_VALUES,
    required: false,
    default_value: Some(TransitionParameterDefault::String("ease-out-cubic")),
    constraint: None,
};

/// The single source of truth for compiled-in transition names and capabilities.
pub static TRANSITION_REGISTRY: &[TransitionDescriptor] = &[
    TransitionDescriptor {
        kind: TransitionKind::Cut,
        name: "cut",
        class: TransitionClass::Immediate,
        scopes: ALL_PUBLIC_SCOPES,
        experimental: false,
        requirements: &[],
        parameters: &[],
    },
    TransitionDescriptor {
        kind: TransitionKind::Fade,
        name: "fade",
        class: TransitionClass::Pairwise,
        scopes: ALL_PUBLIC_SCOPES,
        experimental: false,
        requirements: &[],
        parameters: &[DURATION_PARAMETER, EASING_PARAMETER],
    },
    TransitionDescriptor {
        kind: TransitionKind::Push,
        name: "push",
        class: TransitionClass::Pairwise,
        scopes: ALL_PUBLIC_SCOPES,
        experimental: false,
        requirements: &[],
        parameters: &[
            TransitionParameterDescriptor {
                name: "direction",
                value_type: TransitionParameterType::Enum,
                allowed_values: &["up", "down", "left", "right"],
                required: true,
                default_value: None,
                constraint: None,
            },
            DURATION_PARAMETER,
            EASING_PARAMETER,
            TransitionParameterDescriptor {
                name: "mode",
                value_type: TransitionParameterType::Enum,
                allowed_values: &["portal", "screen", "pan"],
                required: false,
                default_value: Some(TransitionParameterDefault::String("portal")),
                constraint: Some("pan is experimental"),
            },
        ],
    },
    TransitionDescriptor {
        kind: TransitionKind::Canvas,
        name: "canvas",
        class: TransitionClass::Scene,
        scopes: WALLPAPER_ACTION_SCOPE,
        experimental: false,
        requirements: &["wallpaper action history"],
        parameters: &[
            TransitionParameterDescriptor {
                name: "zoom_out_ms",
                value_type: TransitionParameterType::Integer,
                allowed_values: &[],
                required: false,
                default_value: Some(TransitionParameterDefault::Integer(180)),
                constraint: Some("positive integer milliseconds"),
            },
            TransitionParameterDescriptor {
                name: "pan_ms",
                value_type: TransitionParameterType::Integer,
                allowed_values: &[],
                required: false,
                default_value: Some(TransitionParameterDefault::Integer(80)),
                constraint: Some("positive integer milliseconds"),
            },
            TransitionParameterDescriptor {
                name: "zoom_in_ms",
                value_type: TransitionParameterType::Integer,
                allowed_values: &[],
                required: false,
                default_value: Some(TransitionParameterDefault::Integer(260)),
                constraint: Some("positive integer milliseconds"),
            },
            EASING_PARAMETER,
            TransitionParameterDescriptor {
                name: "mode",
                value_type: TransitionParameterType::Enum,
                allowed_values: &["clipped", "morph", "overlap", "collage", "span"],
                required: false,
                default_value: Some(TransitionParameterDefault::String("clipped")),
                constraint: Some("collage and span require walk=strip"),
            },
            TransitionParameterDescriptor {
                name: "walk",
                value_type: TransitionParameterType::Enum,
                allowed_values: &["paged", "strip"],
                required: false,
                default_value: Some(TransitionParameterDefault::String("paged")),
                constraint: None,
            },
            TransitionParameterDescriptor {
                name: "pan_axis",
                value_type: TransitionParameterType::Enum,
                allowed_values: &["auto", "horizontal", "vertical"],
                required: false,
                default_value: Some(TransitionParameterDefault::String("auto")),
                constraint: None,
            },
            TransitionParameterDescriptor {
                name: "overview_scale",
                value_type: TransitionParameterType::Number,
                allowed_values: &[],
                required: false,
                default_value: Some(TransitionParameterDefault::Number(0.333_333)),
                constraint: Some("greater than 0 and at most 1"),
            },
            TransitionParameterDescriptor {
                name: "tile_count",
                value_type: TransitionParameterType::IntegerOrEnum,
                allowed_values: &["auto"],
                required: false,
                default_value: Some(TransitionParameterDefault::String("auto")),
                constraint: Some("fixed integer range 1..=256"),
            },
            TransitionParameterDescriptor {
                name: "max_tile_count",
                value_type: TransitionParameterType::Integer,
                allowed_values: &[],
                required: false,
                default_value: None,
                constraint: Some("integer range 1..=256; applies to tile_count=auto"),
            },
        ],
    },
    TransitionDescriptor {
        kind: TransitionKind::World,
        name: "world",
        class: TransitionClass::Scene,
        scopes: ALL_PUBLIC_SCOPES,
        experimental: true,
        requirements: &[
            "supervisor planning",
            "wallpaper library",
            "ready cache coverage",
        ],
        parameters: &[DURATION_PARAMETER, EASING_PARAMETER],
    },
];

/// Return all compiled-in transition descriptors in stable display order.
#[must_use]
pub fn transition_registry() -> &'static [TransitionDescriptor] {
    TRANSITION_REGISTRY
}

/// Find a compiled-in transition by its stable name.
#[must_use]
pub fn transition_descriptor(name: &str) -> Option<&'static TransitionDescriptor> {
    TRANSITION_REGISTRY
        .iter()
        .find(|descriptor| descriptor.name == name)
}
