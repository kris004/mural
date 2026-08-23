use mural_ipc::CanvasPanAxis;
use mural_render::{
    Easing as RenderEasing, Grid, Rect, Size, canvas_grid_for_overview_axis, eased_progress,
};

use super::canvas_walk_axis;

#[derive(Clone)]
pub(crate) struct CanvasSpan {
    pub(crate) desktop_width: i32,
    pub(crate) desktop_height: i32,
    pub(crate) viewport_x: i32,
    pub(crate) viewport_y: i32,
    pub(crate) viewport_width: i32,
    pub(crate) viewport_height: i32,
    pub(crate) output_index: usize,
    pub(crate) output_count: usize,
    pub(crate) slots: Vec<CanvasSpanSlot>,
}

#[derive(Clone, Copy)]
pub(crate) struct CanvasSpanSlot {
    pub(crate) x: i32,
    pub(crate) y: i32,
    pub(crate) width: i32,
    pub(crate) height: i32,
}

impl CanvasSpan {
    pub(crate) fn output_size(&self) -> Size {
        Size {
            width: u32::try_from(self.desktop_width.max(1)).unwrap_or(1),
            height: u32::try_from(self.desktop_height.max(1)).unwrap_or(1),
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) enum CanvasRasterMode {
    Natural,
    FullThumbnail,
}

impl CanvasRasterMode {
    pub(crate) const fn full_thumbnail(self) -> bool {
        matches!(self, Self::FullThumbnail)
    }
}

pub(crate) struct CanvasRectLayout {
    pub(crate) rects: Vec<Rect>,
    pub(crate) old_index: usize,
    pub(crate) target_index: usize,
    pub(crate) raster: CanvasRasterMode,
}

#[derive(Clone, Copy)]
pub(crate) struct CanvasLayoutInput<'a> {
    pub(crate) tile_count: usize,
    pub(crate) old_index: usize,
    pub(crate) target_index: usize,
    pub(crate) pan_axis: CanvasPanAxis,
    pub(crate) overview_scale: f32,
    pub(crate) output: Size,
    pub(crate) aspects: &'a [f32],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CanvasPackAxis {
    Rows,
    Columns,
}

impl CanvasPackAxis {
    pub(crate) const fn is_horizontal(self) -> bool {
        matches!(self, Self::Rows)
    }
}

pub(crate) fn canvas_morph_pack_axis(output: Size) -> CanvasPackAxis {
    if output.width >= output.height {
        CanvasPackAxis::Rows
    } else {
        CanvasPackAxis::Columns
    }
}

pub(crate) fn canvas_morph_layout(input: CanvasLayoutInput<'_>) -> Option<CanvasRectLayout> {
    let walk_axis = canvas_walk_axis(input.pan_axis, input.output);
    let grid = canvas_grid_for_overview_axis(
        input.tile_count,
        input.output,
        input.overview_scale,
        walk_axis.is_horizontal(),
    );
    let rects = canvas_morph_rects(
        grid,
        input.aspects,
        canvas_morph_pack_axis(input.output).is_horizontal(),
    );
    canvas_rect_layout(input, rects, CanvasRasterMode::Natural)
}

pub(crate) fn canvas_overlap_layout(input: CanvasLayoutInput<'_>) -> Option<CanvasRectLayout> {
    let walk_axis = canvas_walk_axis(input.pan_axis, input.output);
    let grid = canvas_grid_for_overview_axis(
        input.tile_count,
        input.output,
        input.overview_scale,
        walk_axis.is_horizontal(),
    );
    canvas_rect_layout(
        input,
        canvas_overlap_rects(grid, input.aspects),
        CanvasRasterMode::FullThumbnail,
    )
}

pub(crate) fn canvas_collage_layout(input: CanvasLayoutInput<'_>) -> Option<CanvasRectLayout> {
    if input.tile_count == 0 {
        return None;
    }
    let walk_axis = canvas_walk_axis(input.pan_axis, input.output);
    let grid = canvas_grid_for_overview_axis(
        input.tile_count,
        input.output,
        input.overview_scale,
        walk_axis.is_horizontal(),
    );
    let old_index = input.old_index.min(input.tile_count - 1);
    let target_index = input.target_index.min(input.tile_count - 1);
    let mut rects = canvas_collage_rects(
        grid,
        input.aspects,
        old_index,
        target_index,
        walk_axis.is_horizontal(),
        input.overview_scale,
    );
    if !rects.is_empty() {
        expand_canvas_focus_layout(&mut rects, old_index, target_index, input.overview_scale);
    }
    canvas_rect_layout(input, rects, CanvasRasterMode::FullThumbnail)
}

pub(crate) fn canvas_rect_layout(
    input: CanvasLayoutInput<'_>,
    rects: Vec<Rect>,
    raster: CanvasRasterMode,
) -> Option<CanvasRectLayout> {
    if rects.is_empty() {
        return None;
    }
    Some(CanvasRectLayout {
        old_index: input.old_index.min(rects.len() - 1),
        target_index: input.target_index.min(rects.len() - 1),
        rects,
        raster,
    })
}

pub(crate) fn canvas_morph_rects(grid: Grid, aspects: &[f32], horizontal: bool) -> Vec<Rect> {
    if horizontal {
        return canvas_horizontal_morph_rects(grid, aspects);
    }
    canvas_vertical_morph_rects(grid, aspects)
}

#[allow(clippy::cast_precision_loss)]
pub(crate) fn canvas_horizontal_morph_rects(grid: Grid, aspects: &[f32]) -> Vec<Rect> {
    let columns = grid.columns.max(1);
    let mut rects = Vec::with_capacity(aspects.len());
    let mut y = 0.0;
    for row_start in (0..aspects.len()).step_by(columns) {
        let row_end = row_start.saturating_add(columns).min(aspects.len());
        let row = &aspects[row_start..row_end];
        let row_width = columns as f32;
        let aspect_sum = row.iter().sum::<f32>().max(f32::EPSILON);
        let row_height = row_width / aspect_sum;
        let mut x = 0.0;
        for aspect in row {
            let width = aspect.max(f32::EPSILON) * row_height;
            rects.push(Rect {
                x,
                y,
                width,
                height: row_height,
            });
            x += width;
        }
        y += row_height;
    }
    rects
}

#[allow(clippy::cast_precision_loss)]
pub(crate) fn canvas_overlap_rects(grid: Grid, aspects: &[f32]) -> Vec<Rect> {
    let columns = grid.columns.max(1);
    let mut rects = Vec::with_capacity(aspects.len());
    for (index, aspect) in aspects.iter().enumerate() {
        let column = index % columns;
        let row = index / columns;
        let center_x = column as f32 + 0.5;
        let center_y = row as f32 + 0.5;
        rects.push(canvas_full_thumbnail_rect(*aspect, center_x, center_y, 1.0));
    }
    rects
}

pub(crate) fn canvas_collage_rects(
    grid: Grid,
    aspects: &[f32],
    old_index: usize,
    target_index: usize,
    horizontal: bool,
    overview_scale: f32,
) -> Vec<Rect> {
    let thumbnail_scale = canvas_focus_layout_thumbnail_scale(overview_scale);
    canvas_focus_spiral_rects(
        grid,
        aspects,
        old_index,
        target_index,
        horizontal,
        thumbnail_scale,
        |_| 1.0,
    )
}

pub(crate) fn canvas_focus_spiral_rects(
    grid: Grid,
    aspects: &[f32],
    old_index: usize,
    target_index: usize,
    horizontal: bool,
    thumbnail_scale: f32,
    scale_for_index: impl Fn(usize) -> f32,
) -> Vec<Rect> {
    let mut rects = canvas_spiral_centers(
        grid,
        aspects.len(),
        old_index,
        target_index,
        horizontal,
        thumbnail_scale,
    )
    .into_iter()
    .enumerate()
    .zip(aspects.iter().copied())
    .map(|((index, center), aspect)| {
        canvas_full_thumbnail_rect(aspect, center.x, center.y, scale_for_index(index))
    })
    .collect::<Vec<_>>();
    separate_canvas_focus_rects(&mut rects, old_index, target_index);
    rects
}

pub(crate) fn separate_canvas_focus_rects(
    rects: &mut [Rect],
    old_index: usize,
    target_index: usize,
) {
    const FOCUS_MARGIN: f32 = 0.18;

    let Some(old_rect) = rects.get(old_index).copied() else {
        return;
    };
    let Some(target_rect) = rects.get_mut(target_index) else {
        return;
    };
    if old_index == target_index {
        return;
    }

    let old_center = canvas_rect_center(old_rect);
    let target_center = canvas_rect_center(*target_rect);
    let dx = target_center.x - old_center.x;
    let dy = target_center.y - old_center.y;
    if dx.abs() >= dy.abs() {
        if dx >= 0.0 {
            let delta = old_rect.x + old_rect.width + FOCUS_MARGIN - target_rect.x;
            target_rect.x += delta.max(0.0);
        } else {
            let delta = target_rect.x + target_rect.width + FOCUS_MARGIN - old_rect.x;
            target_rect.x -= delta.max(0.0);
        }
    } else if dy >= 0.0 {
        let delta = old_rect.y + old_rect.height + FOCUS_MARGIN - target_rect.y;
        target_rect.y += delta.max(0.0);
    } else {
        let delta = target_rect.y + target_rect.height + FOCUS_MARGIN - old_rect.y;
        target_rect.y -= delta.max(0.0);
    }
}

pub(crate) fn canvas_spiral_centers(
    grid: Grid,
    len: usize,
    old_index: usize,
    target_index: usize,
    horizontal: bool,
    thumbnail_scale: f32,
) -> Vec<CanvasPoint> {
    let target = canvas_target_vector(grid, old_index, target_index, horizontal);
    let spacing = canvas_spiral_spacing(thumbnail_scale);
    (0..len)
        .map(|index| canvas_spiral_center(index, old_index, target_index, target, spacing))
        .collect()
}

#[allow(clippy::cast_precision_loss)]
pub(crate) fn canvas_spiral_center(
    index: usize,
    old_index: usize,
    target_index: usize,
    target: CanvasPoint,
    spacing: f32,
) -> CanvasPoint {
    const GOLDEN_ANGLE: f32 = 2.399_963_1;

    if index == old_index {
        return CanvasPoint { x: 0.0, y: 0.0 };
    }
    if index == target_index {
        return target;
    }

    let signed = isize::try_from(index).unwrap_or(isize::MAX)
        - isize::try_from(old_index).unwrap_or(isize::MAX);
    let rank = signed.unsigned_abs().max(1) as f32;
    let side = if signed < 0 { -1.0 } else { 1.0 };
    let axis_angle = target.y.atan2(target.x);
    let radius = 0.75 + rank.sqrt() * spacing;
    let angle = axis_angle + side * (std::f32::consts::FRAC_PI_2 + rank * GOLDEN_ANGLE);
    let path_bias = if signed < 0 { -0.18 } else { 0.28 };
    CanvasPoint {
        x: target.x * path_bias + angle.cos() * radius,
        y: target.y * path_bias + angle.sin() * radius,
    }
}

pub(crate) fn canvas_focus_layout_thumbnail_scale(overview_scale: f32) -> f32 {
    let visible_tiles = 1.0 / overview_scale.clamp(f32::EPSILON, 1.0);
    (visible_tiles / 4.5).clamp(1.15, 2.0)
}

pub(crate) fn canvas_spiral_spacing(thumbnail_scale: f32) -> f32 {
    (0.68 / thumbnail_scale.sqrt()).clamp(0.38, 0.68)
}

#[allow(clippy::cast_precision_loss)]
pub(crate) fn canvas_target_vector(
    grid: Grid,
    old_index: usize,
    target_index: usize,
    horizontal: bool,
) -> CanvasPoint {
    let columns = grid.columns.max(1);
    let old_column = old_index % columns;
    let old_row = old_index / columns;
    let target_column = target_index % columns;
    let target_row = target_index / columns;
    let mut x = target_column as f32 - old_column as f32;
    let mut y = target_row as f32 - old_row as f32;
    if x.hypot(y) <= f32::EPSILON {
        if horizontal {
            x = 2.0;
        } else {
            y = 2.0;
        }
    }
    let length = x.hypot(y);
    if length < 2.0 {
        let scale = 2.0 / length.max(f32::EPSILON);
        x *= scale;
        y *= scale;
    }
    CanvasPoint { x, y }
}

pub(crate) fn canvas_full_thumbnail_rect(
    aspect: f32,
    center_x: f32,
    center_y: f32,
    scale: f32,
) -> Rect {
    let size = canvas_full_thumbnail_size(aspect, scale);
    Rect {
        x: center_x - size.width / 2.0,
        y: center_y - size.height / 2.0,
        width: size.width,
        height: size.height,
    }
}

pub(crate) fn canvas_full_thumbnail_size(aspect: f32, scale: f32) -> CanvasSize {
    let aspect = aspect.max(f32::EPSILON);
    let scale = scale.max(f32::EPSILON);
    CanvasSize {
        width: aspect.max(1.0) * scale,
        height: (1.0 / aspect).max(1.0) * scale,
    }
}

pub(crate) fn expand_canvas_focus_layout(
    rects: &mut [Rect],
    old_index: usize,
    target_index: usize,
    overview_scale: f32,
) {
    if rects.is_empty() {
        return;
    }
    let bounds = canvas_collage_bounds(rects);
    let needed_extent = (0.5 + 0.12) / overview_scale.max(f32::EPSILON);
    let mut layout_scale: f32 = 1.0;
    for index in [old_index, target_index] {
        let Some(rect) = rects.get(index).copied() else {
            continue;
        };
        let center = canvas_rect_center(rect);
        layout_scale = layout_scale.max(required_canvas_extent_scale(
            center.x - bounds.x,
            needed_extent,
        ));
        layout_scale = layout_scale.max(required_canvas_extent_scale(
            bounds.x + bounds.width - center.x,
            needed_extent,
        ));
        layout_scale = layout_scale.max(required_canvas_extent_scale(
            center.y - bounds.y,
            needed_extent,
        ));
        layout_scale = layout_scale.max(required_canvas_extent_scale(
            bounds.y + bounds.height - center.y,
            needed_extent,
        ));
    }
    if layout_scale <= 1.0 + f32::EPSILON {
        return;
    }

    let origin = canvas_focus_midpoint(rects, old_index, target_index);
    for rect in rects {
        rect.x = origin.x + (rect.x - origin.x) * layout_scale;
        rect.y = origin.y + (rect.y - origin.y) * layout_scale;
        rect.width *= layout_scale;
        rect.height *= layout_scale;
    }
}

pub(crate) fn required_canvas_extent_scale(extent: f32, needed_extent: f32) -> f32 {
    if extent <= f32::EPSILON {
        return 1.0;
    }
    (needed_extent / extent).max(1.0)
}

pub(crate) fn canvas_focus_midpoint(
    rects: &[Rect],
    old_index: usize,
    target_index: usize,
) -> CanvasPoint {
    let old_center = rects
        .get(old_index)
        .copied()
        .map_or(CanvasPoint { x: 0.0, y: 0.0 }, canvas_rect_center);
    let target_center = rects
        .get(target_index)
        .copied()
        .map_or(old_center, canvas_rect_center);
    CanvasPoint {
        x: f32::midpoint(old_center.x, target_center.x),
        y: f32::midpoint(old_center.y, target_center.y),
    }
}

pub(crate) fn canvas_rect_center(rect: Rect) -> CanvasPoint {
    CanvasPoint {
        x: rect.x + rect.width / 2.0,
        y: rect.y + rect.height / 2.0,
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct CanvasPoint {
    pub(crate) x: f32,
    pub(crate) y: f32,
}

#[derive(Clone, Copy)]
pub(crate) struct CanvasSize {
    pub(crate) width: f32,
    pub(crate) height: f32,
}

#[allow(clippy::cast_precision_loss)]
pub(crate) fn canvas_vertical_morph_rects(grid: Grid, aspects: &[f32]) -> Vec<Rect> {
    let rows = grid.rows.max(1);
    let columns = grid.columns.max(1);
    let mut rects = vec![
        Rect {
            x: 0.0,
            y: 0.0,
            width: 0.0,
            height: 0.0,
        };
        aspects.len()
    ];
    let mut x = 0.0;
    for column in 0..columns {
        let indices = (0..rows)
            .map(|row| row.saturating_mul(columns).saturating_add(column))
            .filter(|index| *index < aspects.len())
            .collect::<Vec<_>>();
        if indices.is_empty() {
            continue;
        }
        let column_height = rows as f32;
        let inverse_sum = indices
            .iter()
            .map(|index| 1.0 / aspects[*index].max(f32::EPSILON))
            .sum::<f32>()
            .max(f32::EPSILON);
        let column_width = column_height / inverse_sum;
        let mut y = 0.0;
        for index in indices {
            let height = column_width / aspects[index].max(f32::EPSILON);
            rects[index] = Rect {
                x,
                y,
                width: column_width,
                height,
            };
            y += height;
        }
        x += column_width;
    }
    rects
}

pub(crate) fn canvas_full_thumbnail_draw_order(
    tile_count: usize,
    old_index: usize,
    target_index: usize,
    progress: f32,
    zoom_out_fraction: f32,
    pan_fraction: f32,
) -> Vec<usize> {
    let old_index = old_index.min(tile_count.saturating_sub(1));
    let target_index = target_index.min(tile_count.saturating_sub(1));
    let target_is_active = progress >= zoom_out_fraction + pan_fraction / 2.0;
    let (lower_focus, upper_focus) = if target_is_active {
        (old_index, target_index)
    } else {
        (target_index, old_index)
    };
    let mut order = Vec::with_capacity(tile_count);
    for index in 0..tile_count {
        if index != old_index && index != target_index {
            order.push(index);
        }
    }
    if lower_focus < tile_count {
        order.push(lower_focus);
    }
    if upper_focus < tile_count && upper_focus != lower_focus {
        order.push(upper_focus);
    }
    order
}

pub(crate) fn canvas_span_group_start(
    focus_index: usize,
    tile_count: usize,
    span: &CanvasSpan,
) -> usize {
    if tile_count == 0 {
        return 0;
    }
    let group_len = canvas_span_group_len(tile_count, span);
    let output_index = span.output_index.min(group_len - 1);
    let mut start = focus_index.saturating_sub(output_index);
    if start.saturating_add(group_len) > tile_count {
        start = tile_count.saturating_sub(group_len);
    }
    start
}

pub(crate) fn canvas_span_group_len(tile_count: usize, span: &CanvasSpan) -> usize {
    if tile_count == 0 {
        return 0;
    }
    span.output_count
        .max(span.slots.len())
        .max(1)
        .min(tile_count)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn canvas_span_draw_order(
    tile_count: usize,
    old_index: usize,
    target_index: usize,
    span: &CanvasSpan,
    progress: f32,
    zoom_out_fraction: f32,
    pan_fraction: f32,
) -> Vec<usize> {
    let old_group = canvas_span_group_indices(old_index, tile_count, span);
    let target_group = canvas_span_group_indices(target_index, tile_count, span);
    let target_is_active = progress >= zoom_out_fraction + pan_fraction / 2.0;
    let (lower_group, upper_group) = if target_is_active {
        (&old_group, &target_group)
    } else {
        (&target_group, &old_group)
    };
    let mut order = Vec::with_capacity(tile_count);
    for index in 0..tile_count {
        if !old_group.contains(&index) && !target_group.contains(&index) {
            order.push(index);
        }
    }
    for index in lower_group {
        if *index < tile_count && !order.contains(index) {
            order.push(*index);
        }
    }
    for index in upper_group {
        if *index < tile_count && !order.contains(index) {
            order.push(*index);
        }
    }
    order
}

pub(crate) fn canvas_span_group_indices(
    focus_index: usize,
    tile_count: usize,
    span: &CanvasSpan,
) -> Vec<usize> {
    if tile_count == 0 {
        return Vec::new();
    }
    let start = canvas_span_group_start(focus_index, tile_count, span);
    let group_len = canvas_span_group_len(tile_count, span);
    (start..start.saturating_add(group_len).min(tile_count)).collect()
}

pub(crate) fn canvas_collage_bounds(rects: &[Rect]) -> Rect {
    let Some(first) = rects.first().copied() else {
        return Rect {
            x: 0.0,
            y: 0.0,
            width: 1.0,
            height: 1.0,
        };
    };

    let mut left = first.x;
    let mut top = first.y;
    let mut right = first.x + first.width;
    let mut bottom = first.y + first.height;
    for rect in rects.iter().skip(1) {
        left = left.min(rect.x);
        top = top.min(rect.y);
        right = right.max(rect.x + rect.width);
        bottom = bottom.max(rect.y + rect.height);
    }

    Rect {
        x: left,
        y: top,
        width: (right - left).max(f32::EPSILON),
        height: (bottom - top).max(f32::EPSILON),
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct CanvasModeTransform {
    pub(crate) scale_x: f32,
    pub(crate) scale_y: f32,
    pub(crate) translate_x: f32,
    pub(crate) translate_y: f32,
}

impl CanvasModeTransform {
    pub(crate) const fn identity() -> Self {
        Self {
            scale_x: 1.0,
            scale_y: 1.0,
            translate_x: 0.0,
            translate_y: 0.0,
        }
    }
}

pub(crate) fn centered_canvas_rect_transform_unclamped(
    rect: Rect,
    scale: f32,
) -> CanvasModeTransform {
    let scale = scale.max(f32::EPSILON);
    CanvasModeTransform {
        scale_x: scale,
        scale_y: scale,
        translate_x: 0.5 - (rect.x + rect.width / 2.0) * scale,
        translate_y: 0.5 - (rect.y + rect.height / 2.0) * scale,
    }
}

#[allow(clippy::cast_precision_loss)]
pub(crate) fn centered_canvas_overview_transform(rect: Rect, scale: f32) -> CanvasModeTransform {
    centered_canvas_rect_transform_unclamped(rect, scale)
}

pub(crate) fn canvas_span_group_rect(index: usize, rects: &[Rect], span: &CanvasSpan) -> Rect {
    if rects.is_empty() {
        return Rect {
            x: 0.0,
            y: 0.0,
            width: 1.0,
            height: 1.0,
        };
    }
    let group_len = canvas_span_group_len(rects.len(), span);
    let start = canvas_span_group_start(index, rects.len(), span);
    canvas_collage_bounds(&rects[start..start + group_len])
}

pub(crate) fn canvas_rect_apply_transform(rect: Rect, transform: CanvasModeTransform) -> Rect {
    Rect {
        x: rect.x * transform.scale_x + transform.translate_x,
        y: rect.y * transform.scale_y + transform.translate_y,
        width: rect.width * transform.scale_x,
        height: rect.height * transform.scale_y,
    }
}

pub(crate) fn canvas_gap_rects(group_rect: Rect, covered_rects: &[Rect]) -> Vec<Rect> {
    if covered_rects.is_empty() {
        return Vec::new();
    }

    let mut x_edges = vec![group_rect.x, group_rect.x + group_rect.width];
    let mut y_edges = vec![group_rect.y, group_rect.y + group_rect.height];
    for rect in covered_rects {
        x_edges.push(rect.x.clamp(group_rect.x, group_rect.x + group_rect.width));
        x_edges.push((rect.x + rect.width).clamp(group_rect.x, group_rect.x + group_rect.width));
        y_edges.push(rect.y.clamp(group_rect.y, group_rect.y + group_rect.height));
        y_edges.push((rect.y + rect.height).clamp(group_rect.y, group_rect.y + group_rect.height));
    }
    sort_dedup_canvas_edges(&mut x_edges);
    sort_dedup_canvas_edges(&mut y_edges);

    let mut gaps = Vec::new();
    for x_pair in x_edges.windows(2) {
        for y_pair in y_edges.windows(2) {
            let rect = Rect {
                x: x_pair[0],
                y: y_pair[0],
                width: x_pair[1] - x_pair[0],
                height: y_pair[1] - y_pair[0],
            };
            if rect.width <= f32::EPSILON || rect.height <= f32::EPSILON {
                continue;
            }
            let center = canvas_rect_center(rect);
            if !covered_rects
                .iter()
                .any(|slot| canvas_rect_contains_point(*slot, center))
            {
                gaps.push(rect);
            }
        }
    }
    gaps.sort_by(|left, right| {
        canvas_rect_area(*right)
            .partial_cmp(&canvas_rect_area(*left))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    gaps
}

pub(crate) fn sort_dedup_canvas_edges(edges: &mut Vec<f32>) {
    edges.sort_by(|left, right| left.partial_cmp(right).unwrap_or(std::cmp::Ordering::Equal));
    edges.dedup_by(|left, right| (*left - *right).abs() <= f32::EPSILON);
}

pub(crate) fn canvas_rect_contains_point(rect: Rect, point: CanvasPoint) -> bool {
    point.x >= rect.x - f32::EPSILON
        && point.x <= rect.x + rect.width + f32::EPSILON
        && point.y >= rect.y - f32::EPSILON
        && point.y <= rect.y + rect.height + f32::EPSILON
}

pub(crate) fn canvas_rect_area(rect: Rect) -> f32 {
    rect.width.max(0.0) * rect.height.max(0.0)
}

#[derive(Clone, Copy)]
enum CanvasStickyDirection {
    FromSlot,
    ToSlot,
}

fn canvas_span_sticky_surrounding_rect(
    morph_rect: Rect,
    focus_index: usize,
    rects: &[Rect],
    span: &CanvasSpan,
    progress: f32,
    direction: CanvasStickyDirection,
) -> Rect {
    let group_rect = canvas_span_group_rect(focus_index, rects, span);
    let covered_rects =
        canvas_span_active_group_rects(focus_index, rects, span, progress, direction);
    let gaps = canvas_gap_rects(group_rect, &covered_rects);
    let mut best = None;
    for gap in gaps {
        let Some(candidate) = canvas_sticky_candidate(morph_rect, gap, group_rect) else {
            continue;
        };
        let movement = (candidate.x - morph_rect.x).abs() + (candidate.y - morph_rect.y).abs();
        if best.is_none_or(|(best_movement, _)| movement < best_movement) {
            best = Some((movement, candidate));
        }
    }
    best.map_or(morph_rect, |(_, rect)| rect)
}

fn canvas_span_active_group_rects(
    focus_index: usize,
    rects: &[Rect],
    span: &CanvasSpan,
    progress: f32,
    direction: CanvasStickyDirection,
) -> Vec<Rect> {
    canvas_span_group_indices(focus_index, rects.len(), span)
        .into_iter()
        .filter_map(|index| {
            let morph_rect = rects.get(index).copied()?;
            let slot_rect = canvas_span_focus_slot_rect(index, focus_index, rects, span)?;
            Some(match direction {
                CanvasStickyDirection::FromSlot => {
                    interpolate_canvas_rect(slot_rect, morph_rect, progress)
                }
                CanvasStickyDirection::ToSlot => {
                    interpolate_canvas_rect(morph_rect, slot_rect, progress)
                }
            })
        })
        .collect()
}

pub(crate) fn canvas_sticky_candidate(rect: Rect, gap: Rect, group_rect: Rect) -> Option<Rect> {
    let side = canvas_gap_side(gap, group_rect);
    let mut candidate = rect;
    let movement = match side {
        CanvasGapSide::Top => {
            if canvas_interval_overlap(rect.x, rect.x + rect.width, gap.x, gap.x + gap.width)
                <= f32::EPSILON
                || rect.y + rect.height > group_rect.y + f32::EPSILON
            {
                return None;
            }
            candidate.y = gap.y + gap.height - rect.height;
            (candidate.y - rect.y).abs()
        }
        CanvasGapSide::Bottom => {
            if canvas_interval_overlap(rect.x, rect.x + rect.width, gap.x, gap.x + gap.width)
                <= f32::EPSILON
                || rect.y < group_rect.y + group_rect.height - f32::EPSILON
            {
                return None;
            }
            candidate.y = gap.y;
            (candidate.y - rect.y).abs()
        }
        CanvasGapSide::Left => {
            if canvas_interval_overlap(rect.y, rect.y + rect.height, gap.y, gap.y + gap.height)
                <= f32::EPSILON
                || rect.x + rect.width > group_rect.x + f32::EPSILON
            {
                return None;
            }
            candidate.x = gap.x + gap.width - rect.width;
            (candidate.x - rect.x).abs()
        }
        CanvasGapSide::Right => {
            if canvas_interval_overlap(rect.y, rect.y + rect.height, gap.y, gap.y + gap.height)
                <= f32::EPSILON
                || rect.x < group_rect.x + group_rect.width - f32::EPSILON
            {
                return None;
            }
            candidate.x = gap.x;
            (candidate.x - rect.x).abs()
        }
    };
    let limit = match side {
        CanvasGapSide::Top | CanvasGapSide::Bottom => rect.height * 0.75 + gap.height,
        CanvasGapSide::Left | CanvasGapSide::Right => rect.width * 0.75 + gap.width,
    };
    if movement <= limit + f32::EPSILON {
        Some(candidate)
    } else {
        None
    }
}

#[derive(Clone, Copy)]
enum CanvasGapSide {
    Top,
    Bottom,
    Left,
    Right,
}

fn canvas_gap_side(gap: Rect, group_rect: Rect) -> CanvasGapSide {
    let distances = [
        (CanvasGapSide::Top, (gap.y - group_rect.y).abs()),
        (
            CanvasGapSide::Bottom,
            (group_rect.y + group_rect.height - (gap.y + gap.height)).abs(),
        ),
        (CanvasGapSide::Left, (gap.x - group_rect.x).abs()),
        (
            CanvasGapSide::Right,
            (group_rect.x + group_rect.width - (gap.x + gap.width)).abs(),
        ),
    ];
    distances
        .into_iter()
        .min_by(|(_, left), (_, right)| {
            left.partial_cmp(right).unwrap_or(std::cmp::Ordering::Equal)
        })
        .map_or(CanvasGapSide::Top, |(side, _)| side)
}

pub(crate) fn canvas_interval_overlap(a_start: f32, a_end: f32, b_start: f32, b_end: f32) -> f32 {
    a_end.min(b_end) - a_start.max(b_start)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn canvas_span_morph_rect(
    index: usize,
    morph_rect: Rect,
    old_index: usize,
    target_index: usize,
    rects: &[Rect],
    span: &CanvasSpan,
    progress: f32,
    zoom_out_fraction: f32,
    pan_fraction: f32,
    easing: RenderEasing,
) -> Rect {
    let progress = progress.clamp(0.0, 1.0);
    let zoom_out_end = zoom_out_fraction.clamp(0.0, 1.0);
    let pan_end = (zoom_out_fraction + pan_fraction).clamp(zoom_out_end, 1.0);

    if progress <= zoom_out_end {
        let phase = if zoom_out_end <= f32::EPSILON {
            1.0
        } else {
            progress / zoom_out_end
        };
        let phase = eased_progress(phase, easing);
        return canvas_span_focus_slot_rect(index, old_index, rects, span).map_or(
            canvas_span_sticky_surrounding_rect(
                morph_rect,
                old_index,
                rects,
                span,
                phase,
                CanvasStickyDirection::FromSlot,
            ),
            |slot_rect| interpolate_canvas_rect(slot_rect, morph_rect, phase),
        );
    }

    if progress <= pan_end {
        return morph_rect;
    }

    let phase_len = (1.0 - pan_end).max(f32::EPSILON);
    let phase = (progress - pan_end) / phase_len;
    let phase = eased_progress(phase, easing);
    canvas_span_focus_slot_rect(index, target_index, rects, span).map_or(
        canvas_span_sticky_surrounding_rect(
            morph_rect,
            target_index,
            rects,
            span,
            phase,
            CanvasStickyDirection::ToSlot,
        ),
        |slot_rect| interpolate_canvas_rect(morph_rect, slot_rect, phase),
    )
}

pub(crate) fn canvas_span_focus_slot_rect(
    index: usize,
    focus_index: usize,
    rects: &[Rect],
    span: &CanvasSpan,
) -> Option<Rect> {
    if rects.is_empty() {
        return None;
    }
    let start = canvas_span_group_start(focus_index, rects.len(), span);
    let slot_index = index.checked_sub(start)?;
    if slot_index >= canvas_span_group_len(rects.len(), span) {
        return None;
    }

    let group_rect = canvas_span_group_rect(focus_index, rects, span);
    let slot_rect = canvas_span_slot_rect(span, slot_index);
    Some(Rect {
        x: group_rect.x + slot_rect.x * group_rect.width,
        y: group_rect.y + slot_rect.y * group_rect.height,
        width: slot_rect.width * group_rect.width,
        height: slot_rect.height * group_rect.height,
    })
}

pub(crate) fn interpolate_canvas_rect(from: Rect, to: Rect, progress: f32) -> Rect {
    let progress = progress.clamp(0.0, 1.0);
    Rect {
        x: from.x + (to.x - from.x) * progress,
        y: from.y + (to.y - from.y) * progress,
        width: from.width + (to.width - from.width) * progress,
        height: from.height + (to.height - from.height) * progress,
    }
}

#[allow(clippy::cast_precision_loss)]
pub(crate) fn canvas_span_slot_rect(span: &CanvasSpan, slot_index: usize) -> Rect {
    let slot = span
        .slots
        .get(slot_index)
        .copied()
        .unwrap_or(CanvasSpanSlot {
            x: span.viewport_x,
            y: span.viewport_y,
            width: span.viewport_width,
            height: span.viewport_height,
        });
    let desktop_width = span.desktop_width.max(1) as f32;
    let desktop_height = span.desktop_height.max(1) as f32;
    Rect {
        x: slot.x as f32 / desktop_width,
        y: slot.y as f32 / desktop_height,
        width: slot.width.max(1) as f32 / desktop_width,
        height: slot.height.max(1) as f32 / desktop_height,
    }
}

pub(crate) fn canvas_final_transform(index: usize, rects: &[Rect]) -> CanvasModeTransform {
    let Some(rect) = rects.get(index).copied() else {
        return CanvasModeTransform::identity();
    };
    canvas_final_transform_for_rect(rect)
}

pub(crate) fn canvas_final_transform_for_rect(rect: Rect) -> CanvasModeTransform {
    let scale_x = if rect.width > f32::EPSILON {
        1.0 / rect.width
    } else if rect.height > f32::EPSILON {
        1.0 / rect.height
    } else {
        1.0
    };
    let scale_y = if rect.height > f32::EPSILON {
        1.0 / rect.height
    } else {
        scale_x
    };
    CanvasModeTransform {
        scale_x,
        scale_y,
        translate_x: -rect.x * scale_x,
        translate_y: -rect.y * scale_y,
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn canvas_mode_transform(
    old_final: CanvasModeTransform,
    old_overview: CanvasModeTransform,
    target_overview: CanvasModeTransform,
    target_final: CanvasModeTransform,
    progress: f32,
    zoom_out_fraction: f32,
    pan_fraction: f32,
    easing: RenderEasing,
) -> CanvasModeTransform {
    let progress = progress.clamp(0.0, 1.0);
    let zoom_out_end = zoom_out_fraction.clamp(0.0, 1.0);
    let pan_end = (zoom_out_fraction + pan_fraction).clamp(zoom_out_end, 1.0);

    if progress <= zoom_out_end {
        let phase = if zoom_out_end <= f32::EPSILON {
            1.0
        } else {
            progress / zoom_out_end
        };
        return interpolate_canvas_transform(
            old_final,
            old_overview,
            eased_progress(phase, easing),
        );
    }

    if progress <= pan_end {
        let phase_len = (pan_end - zoom_out_end).max(f32::EPSILON);
        let phase = (progress - zoom_out_end) / phase_len;
        return interpolate_canvas_transform(
            old_overview,
            target_overview,
            eased_progress(phase, easing),
        );
    }

    let phase_len = (1.0 - pan_end).max(f32::EPSILON);
    let phase = (progress - pan_end) / phase_len;
    interpolate_canvas_transform(target_overview, target_final, eased_progress(phase, easing))
}

fn interpolate_canvas_transform(
    from: CanvasModeTransform,
    to: CanvasModeTransform,
    progress: f32,
) -> CanvasModeTransform {
    let progress = progress.clamp(0.0, 1.0);
    CanvasModeTransform {
        scale_x: from.scale_x + (to.scale_x - from.scale_x) * progress,
        scale_y: from.scale_y + (to.scale_y - from.scale_y) * progress,
        translate_x: from.translate_x + (to.translate_x - from.translate_x) * progress,
        translate_y: from.translate_y + (to.translate_y - from.translate_y) * progress,
    }
}
