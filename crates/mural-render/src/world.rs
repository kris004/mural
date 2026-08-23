use std::collections::BTreeMap;

use crate::{Easing, Rect, eased_progress};

/// Stable row-major layout for the full-library virtual world.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WorldLayout {
    pub entry_count: usize,
    pub columns: usize,
    pub rows: usize,
}

impl WorldLayout {
    /// Build a row-major world layout with at least one column.
    #[must_use]
    pub fn new(entry_count: usize, columns: usize) -> Self {
        let columns = columns.max(1);
        Self {
            entry_count,
            columns,
            rows: entry_count.div_ceil(columns),
        }
    }

    /// Return the row-major cell for a library index.
    #[must_use]
    pub fn cell(self, index: usize) -> Option<WorldCell> {
        if index >= self.entry_count {
            return None;
        }
        Some(WorldCell {
            index,
            column: index % self.columns,
            row: index / self.columns,
        })
    }
}

/// Pure snapshot of one ordered wallpaper library for world placement.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorldSnapshot {
    paths: Vec<String>,
    index_by_path: BTreeMap<String, usize>,
    layout: WorldLayout,
}

impl WorldSnapshot {
    /// Capture an ordered library snapshot with a fixed row-major layout.
    #[must_use]
    pub fn new(paths: Vec<String>, columns: usize) -> Self {
        let layout = WorldLayout::new(paths.len(), columns);
        let index_by_path = paths
            .iter()
            .enumerate()
            .map(|(index, path)| (path.clone(), index))
            .collect();
        Self {
            paths,
            index_by_path,
            layout,
        }
    }

    /// Return this snapshot's immutable row-major layout.
    #[must_use]
    pub const fn layout(&self) -> WorldLayout {
        self.layout
    }

    /// Return the stable index for a path in this snapshot.
    #[must_use]
    pub fn index_of(&self, path: &str) -> Option<usize> {
        self.index_by_path.get(path).copied()
    }

    /// Return the path at a stable world index.
    #[must_use]
    pub fn path(&self, index: usize) -> Option<&str> {
        self.paths.get(index).map(String::as_str)
    }

    /// Return the cell for a path in this snapshot.
    #[must_use]
    pub fn cell_for_path(&self, path: &str) -> Option<WorldCell> {
        self.layout.cell(self.index_of(path)?)
    }

    /// Return the unit cell rectangle for a path in this snapshot.
    #[must_use]
    pub fn rect_for_path(&self, path: &str) -> Option<Rect> {
        world_cell_rect(self.layout, self.index_of(path)?)
    }
}

/// One wallpaper's deterministic position in the virtual world.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WorldCell {
    pub index: usize,
    pub column: usize,
    pub row: usize,
}

/// Visible world query in cell units.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VisibleWorldQuery {
    pub world_rect: Rect,
    pub margin_cells: f32,
}

/// Half-open row/column range of visible world cells.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VisibleWorldRange {
    pub start_column: usize,
    pub end_column: usize,
    pub start_row: usize,
    pub end_row: usize,
    pub cell_count: usize,
}

/// One cache tile in the row-major world tile grid.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WorldTile {
    pub column: usize,
    pub row: usize,
}

/// Candidate cache LOD for a bounded world route.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WorldRouteLodCandidate {
    pub lod: usize,
    pub tile_cells: usize,
}

/// Selected cache LOD and tile count for a bounded world route.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WorldRouteLodSelection {
    pub lod: usize,
    pub tile_cells: usize,
    pub tile_count: usize,
}

/// Camera path for a current-to-target world route.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WorldCameraPath {
    pub start: Rect,
    pub overview: Rect,
    pub target: Rect,
}

/// Return the unit cell rectangle for a library index.
#[must_use]
#[allow(clippy::cast_precision_loss)]
pub fn world_cell_rect(layout: WorldLayout, index: usize) -> Option<Rect> {
    let cell = layout.cell(index)?;
    Some(Rect {
        x: cell.column as f32,
        y: cell.row as f32,
        width: 1.0,
        height: 1.0,
    })
}

/// Convert a camera-visible rectangle into a bounded row/column range.
#[must_use]
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
pub fn visible_world_range(layout: WorldLayout, query: VisibleWorldQuery) -> VisibleWorldRange {
    if layout.entry_count == 0 || layout.rows == 0 {
        return VisibleWorldRange {
            start_column: 0,
            end_column: 0,
            start_row: 0,
            end_row: 0,
            cell_count: 0,
        };
    }

    let margin = query.margin_cells.max(0.0);
    let rect = query.world_rect;
    let min_x = rect.x.min(rect.x + rect.width) - margin;
    let max_x = rect.x.max(rect.x + rect.width) + margin;
    let min_y = rect.y.min(rect.y + rect.height) - margin;
    let max_y = rect.y.max(rect.y + rect.height) + margin;

    let start_column = clamp_floor(min_x, layout.columns);
    let end_column = clamp_ceil(max_x, layout.columns);
    let start_row = clamp_floor(min_y, layout.rows);
    let end_row = clamp_ceil(max_y, layout.rows);
    let cell_count = visible_cell_count(layout, start_column, end_column, start_row, end_row);

    VisibleWorldRange {
        start_column,
        end_column,
        start_row,
        end_row,
        cell_count,
    }
}

/// Return cache tiles intersecting a visible world range.
#[must_use]
pub fn world_tiles_for_range(range: VisibleWorldRange, tile_cells: usize) -> Vec<WorldTile> {
    let tile_cells = tile_cells.max(1);
    if range.start_column >= range.end_column || range.start_row >= range.end_row {
        return Vec::new();
    }

    let start_column = range.start_column / tile_cells;
    let end_column = range.end_column.div_ceil(tile_cells);
    let start_row = range.start_row / tile_cells;
    let end_row = range.end_row.div_ceil(tile_cells);
    let mut tiles = Vec::with_capacity(
        end_column
            .saturating_sub(start_column)
            .saturating_mul(end_row.saturating_sub(start_row)),
    );
    for row in start_row..end_row {
        for column in start_column..end_column {
            tiles.push(WorldTile { column, row });
        }
    }
    tiles
}

/// Return cache tiles needed for the rectangular route between two cells.
#[must_use]
#[allow(clippy::cast_precision_loss)]
pub fn world_tiles_for_route(
    layout: WorldLayout,
    start_index: usize,
    target_index: usize,
    tile_cells: usize,
    margin_cells: f32,
) -> Vec<WorldTile> {
    let Some(start) = layout.cell(start_index) else {
        return Vec::new();
    };
    let Some(target) = layout.cell(target_index) else {
        return Vec::new();
    };

    let min_column = start.column.min(target.column) as f32;
    let max_column = start.column.max(target.column).saturating_add(1) as f32;
    let min_row = start.row.min(target.row) as f32;
    let max_row = start.row.max(target.row).saturating_add(1) as f32;
    let range = visible_world_range(
        layout,
        VisibleWorldQuery {
            world_rect: Rect {
                x: min_column,
                y: min_row,
                width: max_column - min_column,
                height: max_row - min_row,
            },
            margin_cells,
        },
    );
    world_tiles_for_range(range, tile_cells)
}

/// Select the first route LOD whose route tile count fits a bounded budget.
#[must_use]
pub fn world_route_lod_for_budget(
    layout: WorldLayout,
    start_index: usize,
    target_index: usize,
    candidates: &[WorldRouteLodCandidate],
    max_tiles: usize,
    margin_cells: f32,
) -> Option<WorldRouteLodSelection> {
    if max_tiles == 0 {
        return None;
    }

    candidates.iter().find_map(|candidate| {
        let tile_count = world_tiles_for_route(
            layout,
            start_index,
            target_index,
            candidate.tile_cells,
            margin_cells,
        )
        .len();
        if tile_count == 0 || tile_count > max_tiles {
            return None;
        }
        Some(WorldRouteLodSelection {
            lod: candidate.lod,
            tile_cells: candidate.tile_cells.max(1),
            tile_count,
        })
    })
}

/// Build a simple zoom-out/pan/zoom-in camera path for a world route.
#[must_use]
#[allow(clippy::cast_precision_loss)]
pub fn world_camera_path(
    layout: WorldLayout,
    start_index: usize,
    target_index: usize,
    margin_cells: f32,
) -> Option<WorldCameraPath> {
    let start = world_cell_rect(layout, start_index)?;
    let target = world_cell_rect(layout, target_index)?;
    let margin = margin_cells.max(0.0);
    let min_x = start.x.min(target.x) - margin;
    let max_x = (start.x + start.width).max(target.x + target.width) + margin;
    let min_y = start.y.min(target.y) - margin;
    let max_y = (start.y + start.height).max(target.y + target.height) + margin;
    let overview = clamp_world_rect(
        Rect {
            x: min_x,
            y: min_y,
            width: max_x - min_x,
            height: max_y - min_y,
        },
        layout,
    );

    Some(WorldCameraPath {
        start,
        overview,
        target,
    })
}

/// Return the visible world rectangle for a simple three-phase camera path.
#[must_use]
pub fn world_camera_view(path: WorldCameraPath, progress: f32, easing: Easing) -> Rect {
    let eased = eased_progress(progress, easing);
    if eased <= 0.5 {
        return interpolate_rect(path.start, path.overview, eased / 0.5);
    }
    interpolate_rect(path.overview, path.target, (eased - 0.5) / 0.5)
}

fn interpolate_rect(from: Rect, to: Rect, t: f32) -> Rect {
    let t = t.clamp(0.0, 1.0);
    Rect {
        x: from.x + (to.x - from.x) * t,
        y: from.y + (to.y - from.y) * t,
        width: from.width + (to.width - from.width) * t,
        height: from.height + (to.height - from.height) * t,
    }
}

#[allow(clippy::cast_precision_loss)]
fn clamp_world_rect(rect: Rect, layout: WorldLayout) -> Rect {
    let max_width = layout.columns.max(1) as f32;
    let max_height = layout.rows.max(1) as f32;
    let width = rect.width.clamp(1.0, max_width);
    let height = rect.height.clamp(1.0, max_height);
    Rect {
        x: rect.x.clamp(0.0, max_width - width),
        y: rect.y.clamp(0.0, max_height - height),
        width,
        height,
    }
}

#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]
fn clamp_floor(value: f32, upper: usize) -> usize {
    if !value.is_finite() || value <= 0.0 {
        return 0;
    }
    if value >= upper as f32 {
        return upper;
    }
    value.floor() as usize
}

#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]
fn clamp_ceil(value: f32, upper: usize) -> usize {
    if !value.is_finite() || value <= 0.0 {
        return 0;
    }
    if value >= upper as f32 {
        return upper;
    }
    value.ceil() as usize
}

fn visible_cell_count(
    layout: WorldLayout,
    start_column: usize,
    end_column: usize,
    start_row: usize,
    end_row: usize,
) -> usize {
    if start_column >= end_column || start_row >= end_row {
        return 0;
    }

    let mut count = 0;
    for row in start_row..end_row {
        let row_start = row
            .saturating_mul(layout.columns)
            .saturating_add(start_column);
        if row_start >= layout.entry_count {
            break;
        }
        let row_end = row
            .saturating_mul(layout.columns)
            .saturating_add(end_column)
            .min(layout.entry_count);
        count += row_end.saturating_sub(row_start);
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn row_major_mapping_is_stable() {
        let layout = WorldLayout::new(10, 4);

        assert_eq!(
            layout.cell(6),
            Some(WorldCell {
                index: 6,
                column: 2,
                row: 1,
            })
        );
        assert_eq!(
            world_cell_rect(layout, 9),
            Some(Rect {
                x: 1.0,
                y: 2.0,
                width: 1.0,
                height: 1.0,
            })
        );
        assert_eq!(layout.cell(10), None);
    }

    #[test]
    fn snapshot_path_lookup_preserves_ordered_library_positions() {
        let snapshot = WorldSnapshot::new(
            vec![
                "/walls/a.jpg".to_owned(),
                "/walls/b.jpg".to_owned(),
                "/walls/c.jpg".to_owned(),
            ],
            2,
        );

        assert_eq!(snapshot.index_of("/walls/b.jpg"), Some(1));
        assert_eq!(snapshot.path(2), Some("/walls/c.jpg"));
        assert_eq!(
            snapshot.cell_for_path("/walls/c.jpg"),
            Some(WorldCell {
                index: 2,
                column: 0,
                row: 1,
            })
        );
        assert_eq!(
            snapshot.rect_for_path("/walls/c.jpg"),
            Some(Rect {
                x: 0.0,
                y: 1.0,
                width: 1.0,
                height: 1.0,
            })
        );
    }

    #[test]
    fn snapshot_world_positions_follow_canonical_order_not_shuffle_order() {
        let canonical = vec![
            "/walls/a.jpg".to_owned(),
            "/walls/b.jpg".to_owned(),
            "/walls/c.jpg".to_owned(),
            "/walls/d.jpg".to_owned(),
        ];
        let shuffle_bag = [
            "/walls/d.jpg".to_owned(),
            "/walls/b.jpg".to_owned(),
            "/walls/a.jpg".to_owned(),
            "/walls/c.jpg".to_owned(),
        ];
        let snapshot = WorldSnapshot::new(canonical, 2);

        assert_eq!(snapshot.index_of(&shuffle_bag[0]), Some(3));
        assert_eq!(snapshot.index_of(&shuffle_bag[2]), Some(0));
        assert_eq!(snapshot.cell_for_path(&shuffle_bag[0]).unwrap().row, 1);
        assert_eq!(snapshot.cell_for_path(&shuffle_bag[2]).unwrap().row, 0);
    }

    #[test]
    fn visible_query_clamps_to_world_bounds() {
        let layout = WorldLayout::new(10, 4);
        let visible = visible_world_range(
            layout,
            VisibleWorldQuery {
                world_rect: Rect {
                    x: -1.0,
                    y: 1.2,
                    width: 3.5,
                    height: 5.0,
                },
                margin_cells: 0.25,
            },
        );

        assert_eq!(
            visible,
            VisibleWorldRange {
                start_column: 0,
                end_column: 3,
                start_row: 0,
                end_row: 3,
                cell_count: 8,
            }
        );
    }

    #[test]
    fn visible_count_ignores_unused_cells_in_last_row() {
        let layout = WorldLayout::new(10, 4);
        let visible = visible_world_range(
            layout,
            VisibleWorldQuery {
                world_rect: Rect {
                    x: 0.0,
                    y: 2.0,
                    width: 4.0,
                    height: 1.0,
                },
                margin_cells: 0.0,
            },
        );

        assert_eq!(visible.cell_count, 2);
    }

    #[test]
    fn small_visible_query_stays_bounded_for_large_library() {
        let layout = WorldLayout::new(100_000, 400);
        let visible = visible_world_range(
            layout,
            VisibleWorldQuery {
                world_rect: Rect {
                    x: 198.5,
                    y: 120.5,
                    width: 3.0,
                    height: 2.0,
                },
                margin_cells: 1.0,
            },
        );

        assert_eq!(visible.start_column, 197);
        assert_eq!(visible.end_column, 203);
        assert_eq!(visible.start_row, 119);
        assert_eq!(visible.end_row, 124);
        assert_eq!(visible.cell_count, 30);
    }

    #[test]
    fn visible_range_maps_to_world_tiles() {
        let tiles = world_tiles_for_range(
            VisibleWorldRange {
                start_column: 7,
                end_column: 17,
                start_row: 0,
                end_row: 9,
                cell_count: 90,
            },
            8,
        );

        assert_eq!(
            tiles,
            vec![
                WorldTile { column: 0, row: 0 },
                WorldTile { column: 1, row: 0 },
                WorldTile { column: 2, row: 0 },
                WorldTile { column: 0, row: 1 },
                WorldTile { column: 1, row: 1 },
                WorldTile { column: 2, row: 1 },
            ]
        );
    }

    #[test]
    fn route_tiles_cover_current_target_and_margin() {
        let layout = WorldLayout::new(1_000, 40);
        let tiles = world_tiles_for_route(layout, 41, 291, 8, 1.0);

        assert_eq!(
            tiles,
            vec![
                WorldTile { column: 0, row: 0 },
                WorldTile { column: 1, row: 0 },
                WorldTile { column: 0, row: 1 },
                WorldTile { column: 1, row: 1 },
            ]
        );
    }

    #[test]
    fn route_lod_selection_prefers_first_candidate_within_tile_budget() {
        let layout = WorldLayout::new(100_000, 400);
        let candidates = [
            WorldRouteLodCandidate {
                lod: 0,
                tile_cells: 8,
            },
            WorldRouteLodCandidate {
                lod: 1,
                tile_cells: 64,
            },
            WorldRouteLodCandidate {
                lod: 2,
                tile_cells: 512,
            },
        ];

        assert_eq!(
            world_route_lod_for_budget(layout, 0, 40_100, &candidates, 16, 1.0),
            Some(WorldRouteLodSelection {
                lod: 1,
                tile_cells: 64,
                tile_count: 4,
            })
        );
    }

    #[test]
    fn route_lod_selection_uses_overview_tiles_for_extreme_jumps() {
        let layout = WorldLayout::new(100_000, 400);
        let candidates = [
            WorldRouteLodCandidate {
                lod: 0,
                tile_cells: 8,
            },
            WorldRouteLodCandidate {
                lod: 1,
                tile_cells: 64,
            },
            WorldRouteLodCandidate {
                lod: 2,
                tile_cells: 512,
            },
        ];

        assert_eq!(
            world_route_lod_for_budget(layout, 0, 99_999, &candidates, 16, 1.0),
            Some(WorldRouteLodSelection {
                lod: 2,
                tile_cells: 512,
                tile_count: 1,
            })
        );
        assert_eq!(
            world_route_lod_for_budget(layout, 0, 99_999, &candidates[..2], 16, 1.0),
            None
        );
    }

    #[test]
    fn invalid_route_indices_return_no_tiles() {
        let layout = WorldLayout::new(10, 4);

        assert!(world_tiles_for_route(layout, 0, 10, 8, 1.0).is_empty());
    }

    #[test]
    fn world_camera_path_covers_route_with_margin() {
        let layout = WorldLayout::new(1_000, 40);
        let path = world_camera_path(layout, 41, 291, 1.0).unwrap();

        assert_eq!(
            path.start,
            Rect {
                x: 1.0,
                y: 1.0,
                width: 1.0,
                height: 1.0,
            }
        );
        assert_eq!(
            path.target,
            Rect {
                x: 11.0,
                y: 7.0,
                width: 1.0,
                height: 1.0,
            }
        );
        assert_eq!(
            path.overview,
            Rect {
                x: 0.0,
                y: 0.0,
                width: 13.0,
                height: 9.0,
            }
        );
    }

    #[test]
    fn world_camera_view_interpolates_through_overview() {
        let path = WorldCameraPath {
            start: Rect {
                x: 2.0,
                y: 4.0,
                width: 1.0,
                height: 1.0,
            },
            overview: Rect {
                x: 0.0,
                y: 0.0,
                width: 10.0,
                height: 8.0,
            },
            target: Rect {
                x: 6.0,
                y: 7.0,
                width: 1.0,
                height: 1.0,
            },
        };

        assert_eq!(world_camera_view(path, 0.0, Easing::Linear), path.start);
        assert_eq!(world_camera_view(path, 0.5, Easing::Linear), path.overview);
        assert_eq!(world_camera_view(path, 1.0, Easing::Linear), path.target);
    }
}
