use crate::{CanvasTransform, Easing, Grid, Size, eased_progress};

/// Compute a compact canvas grid that roughly follows the output aspect ratio.
#[must_use]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]
pub fn canvas_grid(tile_count: usize, output: Size) -> Grid {
    let tile_count = tile_count.max(1);
    let aspect = output.width as f32 / output.height.max(1) as f32;
    let mut best = Grid {
        columns: tile_count,
        rows: 1,
    };
    let mut best_score = f32::MAX;

    for columns in 1..=tile_count {
        let rows = tile_count.div_ceil(columns).max(1);
        let grid_aspect = columns as f32 / rows as f32;
        let unused = columns * rows - tile_count;
        let aspect_score = (grid_aspect / aspect).ln().abs();
        let unused_score = unused as f32 * 0.35;
        let line_score = if rows == 1 && tile_count > 3 {
            0.5
        } else {
            0.0
        };
        let score = aspect_score + unused_score + line_score;
        if score < best_score {
            best_score = score;
            best = Grid { columns, rows };
        }
    }

    best
}

/// Compute a canvas grid for a requested overview scale.
///
/// When enough tiles are available, this preserves the minimum row/column
/// depth needed to center a tile at `overview_scale` without exposing the
/// canvas edge. If the tile count is too small, it falls back to the generic
/// compact grid and callers may expose blank space.
#[must_use]
pub fn canvas_grid_for_overview(tile_count: usize, output: Size, overview_scale: f32) -> Grid {
    canvas_grid_for_overview_axis(
        tile_count,
        output,
        overview_scale,
        output.width >= output.height,
    )
}

/// Compute a canvas grid for a requested overview scale and pan axis.
///
/// When `horizontal` is true, the grid's long axis is horizontal. Otherwise the
/// long axis is vertical.
#[must_use]
pub fn canvas_grid_for_overview_axis(
    tile_count: usize,
    output: Size,
    overview_scale: f32,
    horizontal: bool,
) -> Grid {
    let tile_count = tile_count.max(1);
    let margin = canvas_focus_margin(overview_scale);
    let cross_axis = margin.saturating_mul(2).saturating_add(1);
    let long_axis = cross_axis.saturating_add(1);
    let required = cross_axis.saturating_mul(long_axis);
    if tile_count < required {
        return canvas_grid(tile_count, output);
    }

    if horizontal {
        Grid {
            columns: tile_count.div_ceil(cross_axis).max(long_axis),
            rows: cross_axis,
        }
    } else {
        Grid {
            columns: cross_axis,
            rows: tile_count.div_ceil(cross_axis).max(long_axis),
        }
    }
}

/// Derive a canvas tile count from the requested overview tile scale.
///
/// The returned count is the smallest landscape/portrait-friendly rectangle
/// with enough margin to center the current tile, pan to an adjacent target
/// tile, and keep a full viewport covered at `overview_scale`.
#[must_use]
pub fn auto_canvas_tile_count(overview_scale: f32, output: Size, max: usize) -> usize {
    auto_canvas_tile_count_for_pan(overview_scale, output, max, 1)
}

/// Derive a canvas tile count with room for a farther natural-order pan.
///
/// `pan_tiles` is the expected tile distance between the current and target
/// wallpaper along the long canvas axis. A value of `1` matches
/// [`auto_canvas_tile_count`].
#[must_use]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]
pub fn auto_canvas_tile_count_for_pan(
    overview_scale: f32,
    output: Size,
    max: usize,
    pan_tiles: usize,
) -> usize {
    auto_canvas_tile_count_for_pan_axis(
        overview_scale,
        output,
        max,
        pan_tiles,
        output.width >= output.height,
    )
}

/// Derive a canvas tile count with room for a natural-order pan on an axis.
#[must_use]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]
pub fn auto_canvas_tile_count_for_pan_axis(
    overview_scale: f32,
    _output: Size,
    max: usize,
    pan_tiles: usize,
    horizontal: bool,
) -> usize {
    let margin = canvas_focus_margin(overview_scale);
    let cross_axis = margin.saturating_mul(2).saturating_add(1);
    let long_axis = cross_axis.saturating_add(pan_tiles.max(1).saturating_mul(2));
    let (columns, rows) = if horizontal {
        (long_axis, cross_axis)
    } else {
        (cross_axis, long_axis)
    };
    columns.saturating_mul(rows).clamp(1, max.max(1))
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]
fn canvas_focus_margin(overview_scale: f32) -> usize {
    let visible = 1.0 / overview_scale.clamp(f32::EPSILON, 1.0);
    (visible / 2.0 - 0.5).ceil().max(0.0) as usize
}

/// Compute the canvas transform for a canvas animation.
///
/// The canvas uses tile units: tile `0` is `[0,1]x[0,1]`, tile `1` moves one
/// column to the right, and so on. At progress `0`, `old_index` fills the
/// output. The first phase zooms out while keeping `old_index` centered. The
/// middle phase pans the zoomed-out canvas until `target_index` is centered.
/// The final phase zooms in while keeping `target_index` centered.
#[must_use]
#[allow(clippy::cast_precision_loss, clippy::too_many_arguments)]
pub fn canvas_transform(
    grid: Grid,
    old_index: usize,
    target_index: usize,
    progress: f32,
    overview_scale: f32,
    zoom_out_fraction: f32,
    pan_fraction: f32,
    easing: Easing,
) -> CanvasTransform {
    let columns = grid.columns.max(1);
    let overview_scale = overview_scale.clamp(f32::EPSILON, 1.0);
    let old = centered_tile_transform(columns, old_index, 1.0);
    let old_overview = centered_tile_transform(columns, old_index, overview_scale);
    let target_overview = centered_tile_transform(columns, target_index, overview_scale);
    let target = centered_tile_transform(columns, target_index, 1.0);

    let progress = progress.clamp(0.0, 1.0);
    let zoom_out_end = zoom_out_fraction.clamp(0.0, 1.0);
    let pan_end = (zoom_out_fraction + pan_fraction).clamp(zoom_out_end, 1.0);

    if progress <= zoom_out_end {
        let phase = if zoom_out_end <= f32::EPSILON {
            1.0
        } else {
            progress / zoom_out_end
        };
        return interpolate_transform(old, old_overview, eased_progress(phase, easing));
    }

    if progress <= pan_end {
        let phase_len = (pan_end - zoom_out_end).max(f32::EPSILON);
        let phase = (progress - zoom_out_end) / phase_len;
        return interpolate_transform(old_overview, target_overview, eased_progress(phase, easing));
    }

    let phase_len = (1.0 - pan_end).max(f32::EPSILON);
    let phase = (progress - pan_end) / phase_len;
    interpolate_transform(target_overview, target, eased_progress(phase, easing))
}

#[allow(clippy::cast_precision_loss)]
fn centered_tile_transform(columns: usize, index: usize, scale: f32) -> CanvasTransform {
    let columns = columns.max(1);
    let column = (index % columns) as f32;
    let row = (index / columns) as f32;
    CanvasTransform {
        scale,
        translate_x: 0.5 - (column + 0.5) * scale,
        translate_y: 0.5 - (row + 0.5) * scale,
    }
}

fn interpolate_transform(
    from: CanvasTransform,
    to: CanvasTransform,
    progress: f32,
) -> CanvasTransform {
    let progress = progress.clamp(0.0, 1.0);
    CanvasTransform {
        scale: from.scale + (to.scale - from.scale) * progress,
        translate_x: from.translate_x + (to.translate_x - from.translate_x) * progress,
        translate_y: from.translate_y + (to.translate_y - from.translate_y) * progress,
    }
}
