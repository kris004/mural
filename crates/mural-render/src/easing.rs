use crate::Easing;

/// Clamp progress to `[0, 1]` and apply the selected easing function.
#[must_use]
pub fn eased_progress(progress: f32, easing: Easing) -> f32 {
    let t = progress.clamp(0.0, 1.0);

    match easing {
        Easing::Linear => t,
        Easing::EaseOutCubic => 1.0 - (1.0 - t).powi(3),
        Easing::EaseInOutCubic => {
            if t < 0.5 {
                4.0 * t.powi(3)
            } else {
                1.0 - (-2.0 * t + 2.0).powi(3) / 2.0
            }
        }
    }
}
