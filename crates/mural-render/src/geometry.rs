use crate::{Rect, ScaleMode, Size};

/// Compute where an image should be drawn on an output for the given scale mode.
///
/// `Fill` covers the output and may crop. `Fit` contains the image and may
/// letterbox. `Center` never scales up, but scales down if needed to fit.
/// `Stretch` ignores aspect ratio.
#[must_use]
#[allow(clippy::cast_precision_loss)]
pub fn image_rect(output: Size, image: Size, mode: ScaleMode) -> Rect {
    let out_w = output.width as f32;
    let out_h = output.height as f32;
    let img_w = image.width as f32;
    let img_h = image.height as f32;

    let (width, height) = match mode {
        ScaleMode::Fill => {
            let scale = (out_w / img_w).max(out_h / img_h);
            (img_w * scale, img_h * scale)
        }
        ScaleMode::Fit => {
            let scale = (out_w / img_w).min(out_h / img_h);
            (img_w * scale, img_h * scale)
        }
        ScaleMode::Center => {
            let scale = 1.0_f32.min((out_w / img_w).min(out_h / img_h));
            (img_w * scale, img_h * scale)
        }
        ScaleMode::Stretch => (out_w, out_h),
    };

    Rect {
        x: (out_w - width) / 2.0,
        y: (out_h - height) / 2.0,
        width,
        height,
    }
}
