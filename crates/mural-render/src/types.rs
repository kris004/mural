/// A push transition direction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PushDirection {
    Up,
    Down,
    Left,
    Right,
}

/// Easing curves accepted by the IPC protocol.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Easing {
    Linear,
    EaseOutCubic,
    EaseInOutCubic,
}

/// Supported wallpaper scaling modes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScaleMode {
    Fill,
    Fit,
    Center,
    Stretch,
}

/// A two-dimensional normalized offset, measured in screen widths/heights.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Offset {
    pub x: f32,
    pub y: f32,
}

/// The translated offsets for the old and new image quads during a push.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PushOffsets {
    pub old: Offset,
    pub new: Offset,
}

/// Pixel size with non-zero dimensions.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Size {
    pub width: u32,
    pub height: u32,
}

/// Destination rectangle in output logical pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

/// Column/row count for a canvas wallpaper canvas.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Grid {
    pub columns: usize,
    pub rows: usize,
}

/// Affine transform from canvas canvas tile units to output units.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CanvasTransform {
    pub scale: f32,
    pub translate_x: f32,
    pub translate_y: f32,
}
