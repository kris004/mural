//! A failed render never advances history. Retry only after the *specific*
//! rejected target has been removed, so a bad batch cannot spin or move peers.
use mural_core::wallpaper::{PreparedWallpaperChange, WallpaperControl};
use mural_ipc::{
    WallpaperResponse, response_error_message, response_invalid_image, response_is_error,
};

use crate::decode::ImageLoadError;

const MAX_REJECTIONS: usize = 8;

pub(crate) fn retry<T>(
    mut attempt: impl FnMut() -> Result<Option<T>, String>,
) -> Result<T, String> {
    for _ in 0..MAX_REJECTIONS {
        if let Some(result) = attempt()? {
            return Ok(result);
        }
    }
    Err(format!(
        "auto-quarantined {MAX_REJECTIONS} images; stopping this request"
    ))
}

pub(crate) fn render_result(response: String) -> Result<(), ImageLoadError> {
    if !response_is_error(&response) {
        return Ok(());
    }
    Err(response_invalid_image(&response).map_or_else(
        || ImageLoadError::Other(response_error_message(&response).unwrap_or(response)),
        ImageLoadError::Invalid,
    ))
}

pub(crate) fn finish_render(
    control: &mut WallpaperControl,
    prepared: PreparedWallpaperChange,
    result: Result<(), ImageLoadError>,
) -> Result<Option<WallpaperResponse>, String> {
    let Err(error) = result else {
        return control.commit_wallpaper_change(prepared).map(Some);
    };
    let target_failed = matches!(&error, ImageLoadError::Invalid(image) if prepared.selection.contains(&image.path));
    control.rollback_wallpaper_change(prepared);
    match error {
        ImageLoadError::Invalid(image) if target_failed => {
            move_rejected(control, &image)?;
            Ok(None)
        }
        other => Err(other.into()),
    }
}

fn move_rejected(
    control: &mut WallpaperControl,
    image: &mural_ipc::InvalidImage,
) -> Result<(), String> {
    let dest = control
        .quarantine_invalid_image(image)
        .map_err(|e| format!("{}; auto-quarantine failed: {e}", image.message))?;
    eprintln!(
        "murald: auto-quarantined {} -> {}: {}",
        image.path,
        dest.display(),
        image.message
    );
    Ok(())
}

/// A deferred target may have been superseded since acknowledgement. Remove
/// the rejected file, but don't replace an unrelated, newer displayed set.
pub(crate) fn recover_deferred(
    control: &mut WallpaperControl,
    outputs: &[mural_core::wallpaper::ActiveOutput],
    image: &mural_ipc::InvalidImage,
) -> bool {
    if let Err(message) = move_rejected(control, image) {
        eprintln!("murald: {message}");
        return false;
    }
    !outputs.is_empty()
        && control
            .current_response(outputs)
            .is_ok_and(|state| state.entries.iter().any(|e| e.path == image.path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_stops_on_success_an_unrelated_error_or_the_limit() {
        let mut n = 0;
        assert_eq!(
            retry(|| {
                n += 1;
                Ok((n == 3).then_some(n))
            }),
            Ok(3)
        );
        let mut n = 0;
        assert!(
            retry::<()>(|| {
                n += 1;
                Ok(None)
            })
            .is_err()
        );
        assert_eq!(n, MAX_REJECTIONS);
        let mut n = 0;
        assert_eq!(
            retry::<()>(|| {
                n += 1;
                Err("GPU unavailable".into())
            }),
            Err("GPU unavailable".into())
        );
        assert_eq!(n, 1);
    }
}
