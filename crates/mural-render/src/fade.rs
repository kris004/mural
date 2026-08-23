use crate::{Easing, eased_progress};

/// Source and target weights for a pairwise fade.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FadeWeights {
    pub old: f32,
    pub new: f32,
}

/// Compute complementary scene weights for a fade transition.
///
/// The renderer applies these weights to complete old and new scenes, including
/// their clear-color regions, rather than blending only the image quads.
#[must_use]
pub fn fade_weights(progress: f32, easing: Easing) -> FadeWeights {
    let new = eased_progress(progress, easing);
    FadeWeights {
        old: 1.0 - new,
        new,
    }
}

/// Interpolate two raw RGBA samples using the fade contract.
///
/// Texture samples remain unpremultiplied and are not precomposited over the
/// clear color. This keeps both endpoints identical to normal wallpaper draws,
/// including for images with partial or zero alpha.
#[must_use]
pub fn fade_rgba(old: [f32; 4], new: [f32; 4], progress: f32, easing: Easing) -> [f32; 4] {
    let weights = fade_weights(progress, easing);
    std::array::from_fn(|index| old[index] * weights.old + new[index] * weights.new)
}

#[cfg(test)]
mod tests {
    use super::{fade_rgba, fade_weights};
    use crate::Easing;

    #[test]
    fn fade_weights_are_complementary_at_endpoints_and_midpoint() {
        for progress in [0.0, 0.5, 1.0] {
            let weights = fade_weights(progress, Easing::Linear);
            assert!((weights.old + weights.new - 1.0).abs() < f32::EPSILON);
            assert!((weights.new - progress).abs() < f32::EPSILON);
        }
    }

    #[test]
    fn fade_weights_clamp_progress_for_every_easing() {
        for easing in [Easing::Linear, Easing::EaseOutCubic, Easing::EaseInOutCubic] {
            assert!(fade_weights(-1.0, easing).new.abs() < f32::EPSILON);
            assert!((fade_weights(2.0, easing).new - 1.0).abs() < f32::EPSILON);
        }
    }

    #[test]
    fn transparent_samples_are_unchanged_at_fade_endpoints() {
        let old = [0.9, 0.2, 0.4, 0.0];
        let new = [0.1, 0.8, 0.3, 0.45];

        for (actual, expected) in fade_rgba(old, new, 0.0, Easing::Linear)
            .into_iter()
            .zip(old)
        {
            assert!((actual - expected).abs() < f32::EPSILON);
        }
        for (actual, expected) in fade_rgba(old, new, 1.0, Easing::Linear)
            .into_iter()
            .zip(new)
        {
            assert!((actual - expected).abs() < f32::EPSILON);
        }
    }
}
