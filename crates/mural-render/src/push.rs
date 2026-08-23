use crate::{Easing, Offset, PushDirection, PushOffsets, eased_progress};

/// Compute old/new quad offsets for a flat push transition.
///
/// The offsets are normalized to output dimensions. For example, `old.y == -1`
/// means the old wallpaper has moved one complete output height upward.
#[must_use]
pub fn push_offsets(direction: PushDirection, progress: f32, easing: Easing) -> PushOffsets {
    let p = eased_progress(progress, easing);

    match direction {
        PushDirection::Up => PushOffsets {
            old: Offset { x: 0.0, y: -p },
            new: Offset { x: 0.0, y: 1.0 - p },
        },
        PushDirection::Down => PushOffsets {
            old: Offset { x: 0.0, y: p },
            new: Offset {
                x: 0.0,
                y: -1.0 + p,
            },
        },
        PushDirection::Left => PushOffsets {
            old: Offset { x: -p, y: 0.0 },
            new: Offset { x: 1.0 - p, y: 0.0 },
        },
        PushDirection::Right => PushOffsets {
            old: Offset { x: p, y: 0.0 },
            new: Offset {
                x: -1.0 + p,
                y: 0.0,
            },
        },
    }
}
