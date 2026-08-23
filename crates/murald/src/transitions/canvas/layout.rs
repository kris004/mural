use std::time::Duration;

use mural_ipc::{CanvasMode, CanvasPanAxis, CanvasTileCount, CanvasWalk, MAX_CANVAS_TILE_COUNT};
use mural_render::{
    Grid, Size, auto_canvas_tile_count_for_pan_axis, canvas_grid_for_overview_axis,
};

use crate::{QUEUED_TRANSITION_SPEEDUP, egl_render::WallpaperTexture};

pub(crate) struct CanvasTile {
    pub(crate) path: String,
    pub(crate) texture: Option<WallpaperTexture>,
}

#[derive(Clone)]
pub(crate) struct CanvasPreviewPlan {
    pub(crate) paths: Vec<String>,
    pub(crate) start_index: usize,
}

pub(crate) struct CanvasUpload {
    pub(crate) surface_index: usize,
    pub(crate) image_path: String,
    pub(crate) decode_id: u64,
    pub(crate) ready_tiles: usize,
    pub(crate) tiles: Vec<CanvasTile>,
}

#[derive(Clone, Copy)]
pub(crate) struct CanvasLayoutSpec {
    pub(crate) tile_count: CanvasTileCount,
    pub(crate) mode: CanvasMode,
    pub(crate) walk: CanvasWalk,
    pub(crate) pan_axis: CanvasPanAxis,
    pub(crate) overview_scale: f32,
}

#[derive(Clone, Copy)]
pub(crate) struct CanvasTileBuild<'a> {
    pub(crate) preview_paths: &'a [String],
    pub(crate) preview_start: usize,
    pub(crate) old_path: Option<&'a str>,
    pub(crate) target_path: &'a str,
    pub(crate) layout: CanvasLayoutSpec,
    pub(crate) pan_tiles: usize,
}

#[derive(Clone, Copy)]
pub(crate) struct CanvasTileArrange<'a> {
    pub(crate) preview_start: usize,
    pub(crate) old_path: Option<&'a str>,
    pub(crate) target_path: &'a str,
    pub(crate) layout: CanvasLayoutSpec,
    pub(crate) output: Size,
    pub(crate) pan_tiles: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CanvasAxis {
    Horizontal,
    Vertical,
}

impl CanvasAxis {
    pub(crate) const fn is_horizontal(self) -> bool {
        matches!(self, Self::Horizontal)
    }
}

#[derive(Clone, Copy)]
pub(crate) struct CanvasPhaseFractions {
    pub(crate) zoom_out: f32,
    pub(crate) pan: f32,
}

pub(crate) fn canvas_tile_paths(
    preview_paths: &[String],
    old_path: Option<&str>,
    target_path: &str,
    tile_count: usize,
) -> Vec<String> {
    let tile_count = tile_count.clamp(1, MAX_CANVAS_TILE_COUNT);
    let required = [old_path.unwrap_or(""), target_path]
        .into_iter()
        .filter(|path| !path.is_empty())
        .collect::<Vec<_>>();
    let mut paths = Vec::with_capacity(tile_count);
    for path in preview_paths.iter().filter(|path| !path.is_empty()) {
        if !paths.iter().any(|candidate| candidate == path) {
            paths.push(path.clone());
        }
        if paths.len() >= tile_count {
            break;
        }
    }

    for path in &required {
        if paths.iter().any(|candidate| candidate == path) {
            continue;
        }
        if paths.len() < tile_count {
            paths.push((*path).to_owned());
        } else if let Some(index) = paths
            .iter()
            .rposition(|candidate| !required.iter().any(|required| required == candidate))
        {
            (*path).clone_into(&mut paths[index]);
        } else {
            paths.push((*path).to_owned());
        }
    }

    while paths.len() > tile_count {
        if let Some(index) = paths
            .iter()
            .rposition(|candidate| !required.iter().any(|required| required == candidate))
        {
            paths.remove(index);
        } else {
            paths.remove(0);
        }
    }
    paths
}

pub(crate) fn arrange_canvas_tile_paths(
    paths: Vec<String>,
    spec: CanvasTileArrange<'_>,
) -> Vec<String> {
    if paths.len() <= 1 {
        return paths;
    }

    let walk_axis = canvas_walk_axis(spec.layout.pan_axis, spec.output);
    let grid = canvas_grid_for_overview_axis(
        paths.len(),
        spec.output,
        spec.layout.overview_scale,
        walk_axis.is_horizontal(),
    );
    let mut arranged = vec![None; paths.len()];

    let anchor_index = spec
        .old_path
        .filter(|path| !path.is_empty())
        .and_then(|path| paths.iter().position(|candidate| candidate == path))
        .or_else(|| {
            paths
                .iter()
                .position(|candidate| candidate == spec.target_path)
        });
    let strip_anchor_slot =
        canvas_focus_slot(grid, spec.layout.overview_scale, spec.pan_tiles, walk_axis);

    for (index, path) in paths.into_iter().enumerate() {
        let preferred_slot = match (spec.layout.mode, spec.layout.walk, anchor_index) {
            (CanvasMode::Span, CanvasWalk::Strip, _) => {
                natural_canvas_slot(index, grid, arranged.len(), walk_axis)
            }
            (_, CanvasWalk::Strip, Some(anchor_index)) => {
                let offset = natural_canvas_offset(index, anchor_index, arranged.len());
                canvas_slot_for_offset(strip_anchor_slot, offset, grid, arranged.len(), walk_axis)
            }
            (_, CanvasWalk::Strip, None) | (_, CanvasWalk::Paged, _) => {
                let natural_index = spec.preview_start.saturating_add(index);
                natural_canvas_slot(natural_index, grid, arranged.len(), walk_axis)
            }
        };
        let slot = nearest_free_canvas_slot(&arranged, preferred_slot);
        arranged[slot] = Some(path);
    }

    arranged.into_iter().flatten().collect()
}

// Shared canvas walk code. Be careful changing these helpers: clipped, morph,
// overlap, collage, and span all consume this tile order.
#[allow(clippy::cast_precision_loss, clippy::cast_sign_loss)]
fn canvas_focus_slot(
    grid: Grid,
    overview_scale: f32,
    pan_tiles: usize,
    walk_axis: CanvasAxis,
) -> usize {
    let columns = grid.columns.max(1);
    let rows = grid.rows.max(1);
    let margin = canvas_focus_margin(overview_scale);
    let pan_tiles = pan_tiles.max(1);
    let (old_column, old_row) = if walk_axis.is_horizontal() {
        (
            margin.saturating_add(pan_tiles).min(columns - 1),
            margin.min(rows - 1),
        )
    } else {
        (
            margin.min(columns - 1),
            margin.saturating_add(pan_tiles).min(rows - 1),
        )
    };

    old_row.saturating_mul(columns).saturating_add(old_column)
}

fn natural_canvas_offset(index: usize, anchor_index: usize, len: usize) -> isize {
    if len == 0 {
        return 0;
    }
    let forward = (index + len - anchor_index) % len;
    if forward > len / 2 {
        -isize::try_from(len - forward).unwrap_or(isize::MAX)
    } else {
        isize::try_from(forward).unwrap_or(isize::MAX)
    }
}

fn canvas_slot_for_offset(
    anchor_slot: usize,
    offset: isize,
    grid: Grid,
    len: usize,
    walk_axis: CanvasAxis,
) -> usize {
    if len == 0 {
        return 0;
    }
    let columns = grid.columns.max(1);
    let preferred = if walk_axis.is_horizontal() {
        add_canvas_slot_offset(anchor_slot, offset, 1)
    } else {
        add_canvas_slot_offset(anchor_slot, offset, columns)
    };
    preferred.min(len - 1)
}

fn add_canvas_slot_offset(anchor_slot: usize, offset: isize, stride: usize) -> usize {
    let anchor_slot = isize::try_from(anchor_slot).unwrap_or(0);
    let stride = isize::try_from(stride).unwrap_or(isize::MAX);
    usize::try_from(anchor_slot.saturating_add(offset.saturating_mul(stride))).unwrap_or(0)
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

fn natural_canvas_slot(
    natural_index: usize,
    grid: Grid,
    len: usize,
    walk_axis: CanvasAxis,
) -> usize {
    if len == 0 {
        return 0;
    }
    let columns = grid.columns.max(1);
    let rows = grid.rows.max(1);
    let local = natural_index % len;
    if walk_axis.is_horizontal() {
        return local;
    }

    let row = local % rows;
    let column = local / rows;
    row.saturating_mul(columns).saturating_add(column)
}

fn nearest_free_canvas_slot(slots: &[Option<String>], preferred: usize) -> usize {
    if slots.is_empty() {
        return 0;
    }
    let preferred = preferred.min(slots.len() - 1);
    if slots[preferred].is_none() {
        return preferred;
    }
    for distance in 1..slots.len() {
        if let Some(left) = preferred.checked_sub(distance)
            && slots[left].is_none()
        {
            return left;
        }
        let right = preferred + distance;
        if right < slots.len() && slots[right].is_none() {
            return right;
        }
    }
    preferred
}

pub(crate) fn resolve_canvas_tile_count_for_pan(
    tile_count: CanvasTileCount,
    overview_scale: f32,
    output: Size,
    pan_tiles: usize,
    pan_axis: CanvasPanAxis,
) -> usize {
    match tile_count {
        CanvasTileCount::Fixed(count) => count.clamp(1, MAX_CANVAS_TILE_COUNT),
        CanvasTileCount::Auto { max } => {
            let max = max
                .unwrap_or(MAX_CANVAS_TILE_COUNT)
                .clamp(1, MAX_CANVAS_TILE_COUNT);
            auto_canvas_tile_count_for_pan_axis(
                overview_scale,
                output,
                max,
                pan_tiles,
                canvas_pan_axis_is_horizontal(pan_axis, output),
            )
        }
    }
}

pub(crate) fn canvas_pan_axis_is_horizontal(pan_axis: CanvasPanAxis, output: Size) -> bool {
    canvas_walk_axis(pan_axis, output).is_horizontal()
}

pub(crate) fn canvas_walk_axis(pan_axis: CanvasPanAxis, output: Size) -> CanvasAxis {
    match pan_axis {
        CanvasPanAxis::Auto if output.width >= output.height => CanvasAxis::Horizontal,
        CanvasPanAxis::Horizontal => CanvasAxis::Horizontal,
        CanvasPanAxis::Auto | CanvasPanAxis::Vertical => CanvasAxis::Vertical,
    }
}

pub(crate) fn canvas_ready_tile_count(
    tiles: &[CanvasTile],
    old_path: Option<&str>,
    target_path: &str,
) -> usize {
    tiles
        .iter()
        .filter(|tile| {
            tile.texture.is_some()
                || Some(tile.path.as_str()) == old_path
                || tile.path == target_path
        })
        .count()
}

pub(crate) fn ensure_canvas_path<T: CanvasPathList>(paths: &mut T, path: &str) {
    paths.ensure_path(path);
}

pub(crate) trait CanvasPathList {
    fn ensure_path(&mut self, path: &str);
}

impl CanvasPathList for Vec<String> {
    fn ensure_path(&mut self, path: &str) {
        if path.is_empty() || self.iter().any(|candidate| candidate == path) {
            return;
        }
        if self.is_empty() {
            self.push(path.to_owned());
        } else {
            let index = self.len() - 1;
            path.clone_into(&mut self[index]);
        }
    }
}

impl CanvasPathList for Vec<CanvasTile> {
    fn ensure_path(&mut self, path: &str) {
        if path.is_empty() || self.iter().any(|candidate| candidate.path == path) {
            return;
        }
        self.push(CanvasTile {
            path: path.to_owned(),
            texture: None,
        });
    }
}

pub(crate) fn canvas_path_index(tiles: &[CanvasTile], path: &str) -> Option<usize> {
    tiles.iter().position(|tile| tile.path == path)
}

pub(crate) fn canvas_duration(zoom_out_ms: u64, pan_ms: u64, zoom_in_ms: u64) -> Duration {
    Duration::from_millis(
        zoom_out_ms
            .saturating_add(pan_ms)
            .saturating_add(zoom_in_ms)
            .max(1),
    )
}

pub(crate) fn accelerated_canvas_phases(
    zoom_out_ms: u64,
    pan_ms: u64,
    zoom_in_ms: u64,
) -> (u64, u64, u64) {
    let speedup = u64::from(QUEUED_TRANSITION_SPEEDUP);
    (
        (zoom_out_ms / speedup).max(1),
        (pan_ms / speedup).max(1),
        (zoom_in_ms / speedup).max(1),
    )
}

pub(crate) fn canvas_phase_fractions(
    zoom_out_ms: u64,
    pan_ms: u64,
    zoom_in_ms: u64,
) -> CanvasPhaseFractions {
    let total = zoom_out_ms
        .saturating_add(pan_ms)
        .saturating_add(zoom_in_ms)
        .max(1);
    #[allow(clippy::cast_precision_loss)]
    CanvasPhaseFractions {
        zoom_out: zoom_out_ms as f32 / total as f32,
        pan: pan_ms as f32 / total as f32,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    fn arrange(
        paths: Vec<String>,
        preview_start: usize,
        old_path: Option<&str>,
        target_path: &str,
        walk: CanvasWalk,
        pan_axis: CanvasPanAxis,
        output: Size,
    ) -> Vec<String> {
        arrange_canvas_tile_paths(
            paths,
            CanvasTileArrange {
                preview_start,
                old_path,
                target_path,
                layout: CanvasLayoutSpec {
                    tile_count: CanvasTileCount::Auto { max: None },
                    mode: CanvasMode::Morph,
                    walk,
                    pan_axis,
                    overview_scale: 0.25,
                },
                output,
                pan_tiles: 3,
            },
        )
    }

    #[test]
    fn canvas_tile_paths_preserve_old_and_target_when_preview_is_full() {
        let tiles = canvas_tile_paths(&paths(&["a", "b", "c"]), Some("old"), "target", 3);

        assert!(tiles.iter().any(|path| path == "old"));
        assert!(tiles.iter().any(|path| path == "target"));
        assert_eq!(tiles.len(), 3);
    }

    #[test]
    fn canvas_tile_paths_deduplicate_preview_entries() {
        let tiles = canvas_tile_paths(&paths(&["a", "a", "b", "target"]), Some("a"), "target", 4);

        assert_eq!(tiles, paths(&["a", "b", "target"]));
    }

    #[test]
    fn arrange_canvas_tile_paths_preserves_absolute_horizontal_phase() {
        let source = (0..55)
            .map(|index| format!("wall-{index}"))
            .collect::<Vec<_>>();

        let arranged = arrange(
            source,
            9,
            None,
            "wall-1",
            CanvasWalk::Paged,
            CanvasPanAxis::Auto,
            Size {
                width: 1920,
                height: 1080,
            },
        );

        assert_eq!(arranged[9], "wall-0");
        assert_eq!(arranged[10], "wall-1");
        assert_eq!(arranged[11], "wall-2");
        assert_eq!(arranged.len(), 55);
    }

    #[test]
    fn arrange_canvas_tile_paths_wraps_forward_motion_to_next_row() {
        let mut source = (0..55)
            .map(|index| format!("wall-{index}"))
            .collect::<Vec<_>>();
        source[0] = "old".to_owned();
        source[3] = "target".to_owned();

        let arranged = arrange(
            source,
            9,
            Some("old"),
            "target",
            CanvasWalk::Paged,
            CanvasPanAxis::Auto,
            Size {
                width: 1920,
                height: 1080,
            },
        );

        let old = arranged.iter().position(|path| path == "old").unwrap();
        let target = arranged.iter().position(|path| path == "target").unwrap();
        let columns = 11;
        assert_eq!(old, 9);
        assert_eq!(target, 12);
        assert!(target / columns > old / columns);
        assert!(target % columns < old % columns);
    }

    #[test]
    fn arrange_canvas_tile_paths_strip_keeps_target_on_same_row() {
        let mut source = (0..55)
            .map(|index| format!("wall-{index}"))
            .collect::<Vec<_>>();
        source[0] = "old".to_owned();
        source[3] = "target".to_owned();

        let arranged = arrange(
            source,
            9,
            Some("old"),
            "target",
            CanvasWalk::Strip,
            CanvasPanAxis::Auto,
            Size {
                width: 1920,
                height: 1080,
            },
        );

        let old = arranged.iter().position(|path| path == "old").unwrap();
        let target = arranged.iter().position(|path| path == "target").unwrap();
        let columns = 11;
        assert_eq!(old / columns, target / columns);
        assert_eq!(target, old + 3);
    }

    #[test]
    fn arrange_canvas_tile_paths_changes_phase_as_history_advances() {
        let source = (0..55)
            .map(|index| format!("wall-{index}"))
            .collect::<Vec<_>>();

        let arranged = arrange(
            source,
            10,
            None,
            "wall-1",
            CanvasWalk::Paged,
            CanvasPanAxis::Auto,
            Size {
                width: 1920,
                height: 1080,
            },
        );

        assert_eq!(arranged[10], "wall-0");
        assert_eq!(arranged[11], "wall-1");
        assert_eq!(arranged[12], "wall-2");
        assert_eq!(arranged.len(), 55);
    }

    #[test]
    fn arrange_canvas_tile_paths_preserves_absolute_vertical_phase() {
        let source = (0..55)
            .map(|index| format!("wall-{index}"))
            .collect::<Vec<_>>();

        let arranged = arrange(
            source,
            9,
            None,
            "wall-1",
            CanvasWalk::Paged,
            CanvasPanAxis::Auto,
            Size {
                width: 1080,
                height: 1920,
            },
        );

        assert_eq!(arranged[45], "wall-0");
        assert_eq!(arranged[50], "wall-1");
        assert_eq!(arranged[1], "wall-2");
        assert_eq!(arranged.len(), 55);
    }

    #[test]
    fn auto_canvas_tile_count_uses_optional_cap() {
        let output = Size {
            width: 1920,
            height: 1080,
        };

        assert_eq!(
            resolve_canvas_tile_count_for_pan(
                CanvasTileCount::Auto { max: None },
                1.0 / 3.0,
                output,
                1,
                CanvasPanAxis::Auto,
            ),
            15
        );
        assert_eq!(
            resolve_canvas_tile_count_for_pan(
                CanvasTileCount::Auto { max: None },
                0.25,
                output,
                1,
                CanvasPanAxis::Auto,
            ),
            35
        );
        assert_eq!(
            resolve_canvas_tile_count_for_pan(
                CanvasTileCount::Auto { max: Some(16) },
                0.25,
                output,
                1,
                CanvasPanAxis::Auto,
            ),
            16
        );
        assert_eq!(
            resolve_canvas_tile_count_for_pan(
                CanvasTileCount::Fixed(5),
                1.0 / 3.0,
                output,
                1,
                CanvasPanAxis::Auto,
            ),
            5
        );
    }

    #[test]
    fn arrange_canvas_tile_paths_honors_horizontal_pan_axis_on_portrait_outputs() {
        let mut source = (0..55)
            .map(|index| format!("wall-{index}"))
            .collect::<Vec<_>>();
        source[0] = "old".to_owned();
        source[3] = "target".to_owned();

        let arranged = arrange(
            source,
            9,
            Some("old"),
            "target",
            CanvasWalk::Strip,
            CanvasPanAxis::Horizontal,
            Size {
                width: 1080,
                height: 1920,
            },
        );

        let old = arranged.iter().position(|path| path == "old").unwrap();
        let target = arranged.iter().position(|path| path == "target").unwrap();
        let columns = 11;
        assert_eq!(old / columns, target / columns);
        assert_eq!(target, old + 3);
    }

    #[test]
    fn arrange_canvas_tile_paths_honors_vertical_pan_axis_on_landscape_outputs() {
        let mut source = (0..55)
            .map(|index| format!("wall-{index}"))
            .collect::<Vec<_>>();
        source[0] = "old".to_owned();
        source[3] = "target".to_owned();

        let arranged = arrange(
            source,
            9,
            Some("old"),
            "target",
            CanvasWalk::Strip,
            CanvasPanAxis::Vertical,
            Size {
                width: 1920,
                height: 1080,
            },
        );

        let old = arranged.iter().position(|path| path == "old").unwrap();
        let target = arranged.iter().position(|path| path == "target").unwrap();
        let columns = 5;
        assert_eq!(old % columns, target % columns);
        assert_eq!(target, old + columns * 3);
    }

    #[test]
    fn canvas_pan_axis_can_override_output_orientation() {
        let portrait = Size {
            width: 1080,
            height: 1920,
        };

        assert!(canvas_pan_axis_is_horizontal(
            CanvasPanAxis::Horizontal,
            portrait,
        ));
        assert!(!canvas_pan_axis_is_horizontal(
            CanvasPanAxis::Vertical,
            portrait,
        ));
    }
}
