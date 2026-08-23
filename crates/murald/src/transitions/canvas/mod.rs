use std::time::{Duration, Instant};

use mural_ipc::{CanvasMode, CanvasPanAxis, CanvasTileCount, CanvasWalk, Easing};

pub(crate) mod cache;
pub(crate) mod layout;
pub(crate) mod render_layout;

mod apply;

pub(crate) use cache::{CanvasCache, CanvasCacheResult, clear_canvas_cache_root};
pub(crate) use layout::{
    CanvasLayoutSpec, CanvasPreviewPlan, CanvasTile, CanvasTileArrange, CanvasTileBuild,
    CanvasUpload, accelerated_canvas_phases, arrange_canvas_tile_paths, canvas_duration,
    canvas_pan_axis_is_horizontal, canvas_path_index, canvas_phase_fractions,
    canvas_ready_tile_count, canvas_tile_paths, canvas_walk_axis, ensure_canvas_path,
    resolve_canvas_tile_count_for_pan,
};
pub(crate) use render_layout::{
    CanvasLayoutInput, CanvasModeTransform, CanvasRectLayout, CanvasSpan, CanvasSpanSlot,
    canvas_collage_layout, canvas_final_transform, canvas_final_transform_for_rect,
    canvas_full_thumbnail_draw_order, canvas_mode_transform, canvas_morph_layout,
    canvas_morph_rects, canvas_overlap_layout, canvas_rect_apply_transform, canvas_span_draw_order,
    canvas_span_group_rect, canvas_span_morph_rect, centered_canvas_overview_transform,
};
#[cfg(test)]
pub(crate) use render_layout::{
    CanvasPoint, canvas_collage_bounds, canvas_collage_rects, canvas_gap_rects,
    canvas_horizontal_morph_rects, canvas_morph_pack_axis, canvas_overlap_rects,
    canvas_rect_center, canvas_span_focus_slot_rect, canvas_span_slot_rect,
    canvas_vertical_morph_rects, expand_canvas_focus_layout,
};

#[derive(Clone, Copy)]
pub(crate) struct Spec {
    pub(crate) zoom_out_ms: u64,
    pub(crate) pan_ms: u64,
    pub(crate) zoom_in_ms: u64,
    pub(crate) easing: Easing,
    pub(crate) mode: CanvasMode,
    pub(crate) walk: CanvasWalk,
    pub(crate) pan_axis: CanvasPanAxis,
    pub(crate) overview_scale: f32,
    pub(crate) tile_count: CanvasTileCount,
    pub(crate) started_at: Instant,
    pub(crate) accelerated: bool,
}

impl Spec {
    pub(crate) fn duration(self) -> Duration {
        canvas_duration(self.zoom_out_ms, self.pan_ms, self.zoom_in_ms)
    }
}

pub(crate) struct Active {
    pub(crate) easing: Easing,
    pub(crate) tiles: Vec<CanvasTile>,
    pub(crate) old_index: usize,
    pub(crate) target_index: usize,
    pub(crate) zoom_out_ms: u64,
    pub(crate) pan_ms: u64,
    pub(crate) zoom_in_ms: u64,
    pub(crate) mode: CanvasMode,
    pub(crate) pan_axis: CanvasPanAxis,
    pub(crate) overview_scale: f32,
    pub(crate) target_decode_id: Option<u64>,
    pub(crate) accelerated: bool,
}

#[derive(Clone)]
pub(crate) struct Queued {
    pub(crate) zoom_out_ms: u64,
    pub(crate) pan_ms: u64,
    pub(crate) zoom_in_ms: u64,
    pub(crate) easing: Easing,
    pub(crate) mode: CanvasMode,
    pub(crate) walk: CanvasWalk,
    pub(crate) pan_axis: CanvasPanAxis,
    pub(crate) overview_scale: f32,
    pub(crate) tile_count: CanvasTileCount,
    pub(crate) preview_paths: Vec<String>,
    pub(crate) preview_start: usize,
}
