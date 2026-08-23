//! Pure rendering math used by mural.
//!
//! This crate deliberately contains no Wayland, EGL, or OpenGL calls. Keeping
//! transition and placement math pure makes it easy to test the visual contract
//! before it is wired into a live renderer.

mod canvas;
mod easing;
mod fade;
mod geometry;
mod push;
mod types;
mod world;

pub use canvas::{
    auto_canvas_tile_count, auto_canvas_tile_count_for_pan, auto_canvas_tile_count_for_pan_axis,
    canvas_grid, canvas_grid_for_overview, canvas_grid_for_overview_axis, canvas_transform,
};
pub use easing::eased_progress;
pub use fade::{FadeWeights, fade_rgba, fade_weights};
pub use geometry::image_rect;
pub use push::push_offsets;
pub use types::{
    CanvasTransform, Easing, Grid, Offset, PushDirection, PushOffsets, Rect, ScaleMode, Size,
};
pub use world::{
    VisibleWorldQuery, VisibleWorldRange, WorldCameraPath, WorldCell, WorldLayout,
    WorldRouteLodCandidate, WorldRouteLodSelection, WorldSnapshot, WorldTile, visible_world_range,
    world_camera_path, world_camera_view, world_cell_rect, world_route_lod_for_budget,
    world_tiles_for_range, world_tiles_for_route,
};

#[cfg(test)]
mod tests {
    use super::*;

    const EPSILON: f32 = 0.000_01;

    fn assert_near(actual: f32, expected: f32) {
        assert!(
            (actual - expected).abs() < EPSILON,
            "expected {actual} to be near {expected}"
        );
    }

    fn assert_offset_near(actual: Offset, expected: Offset) {
        assert_near(actual.x, expected.x);
        assert_near(actual.y, expected.y);
    }

    #[test]
    fn push_up_offsets_match_plan() {
        let start = push_offsets(PushDirection::Up, 0.0, Easing::Linear);
        assert_offset_near(start.old, Offset { x: 0.0, y: 0.0 });
        assert_offset_near(start.new, Offset { x: 0.0, y: 1.0 });

        let middle = push_offsets(PushDirection::Up, 0.5, Easing::Linear);
        assert_offset_near(middle.old, Offset { x: 0.0, y: -0.5 });
        assert_offset_near(middle.new, Offset { x: 0.0, y: 0.5 });

        let end = push_offsets(PushDirection::Up, 1.0, Easing::Linear);
        assert_offset_near(end.old, Offset { x: 0.0, y: -1.0 });
        assert_offset_near(end.new, Offset { x: 0.0, y: 0.0 });
    }

    #[test]
    fn all_push_directions_match_plan_at_half_progress() {
        let cases = [
            (
                PushDirection::Down,
                Offset { x: 0.0, y: 0.5 },
                Offset { x: 0.0, y: -0.5 },
            ),
            (
                PushDirection::Left,
                Offset { x: -0.5, y: 0.0 },
                Offset { x: 0.5, y: 0.0 },
            ),
            (
                PushDirection::Right,
                Offset { x: 0.5, y: 0.0 },
                Offset { x: -0.5, y: 0.0 },
            ),
        ];

        for (direction, old, new) in cases {
            let offsets = push_offsets(direction, 0.5, Easing::Linear);
            assert_offset_near(offsets.old, old);
            assert_offset_near(offsets.new, new);
        }
    }

    #[test]
    fn easing_clamps_progress() {
        assert_near(eased_progress(-1.0, Easing::Linear), 0.0);
        assert_near(eased_progress(2.0, Easing::Linear), 1.0);
    }

    #[test]
    fn cubic_easing_has_expected_midpoints() {
        assert_near(eased_progress(0.5, Easing::EaseOutCubic), 0.875);
        assert_near(eased_progress(0.5, Easing::EaseInOutCubic), 0.5);
    }

    #[test]
    fn fill_covers_output_and_crops_overflow() {
        let rect = image_rect(
            Size {
                width: 1920,
                height: 1080,
            },
            Size {
                width: 1000,
                height: 1000,
            },
            ScaleMode::Fill,
        );

        assert_near(rect.x, 0.0);
        assert_near(rect.y, -420.0);
        assert_near(rect.width, 1920.0);
        assert_near(rect.height, 1920.0);
    }

    #[test]
    fn fit_contains_image_and_letterboxes() {
        let rect = image_rect(
            Size {
                width: 1920,
                height: 1080,
            },
            Size {
                width: 1000,
                height: 1000,
            },
            ScaleMode::Fit,
        );

        assert_near(rect.x, 420.0);
        assert_near(rect.y, 0.0);
        assert_near(rect.width, 1080.0);
        assert_near(rect.height, 1080.0);
    }

    #[test]
    fn center_does_not_scale_up() {
        let rect = image_rect(
            Size {
                width: 1920,
                height: 1080,
            },
            Size {
                width: 320,
                height: 200,
            },
            ScaleMode::Center,
        );

        assert_near(rect.x, 800.0);
        assert_near(rect.y, 440.0);
        assert_near(rect.width, 320.0);
        assert_near(rect.height, 200.0);
    }

    #[test]
    fn canvas_grid_follows_landscape_output() {
        assert_eq!(
            canvas_grid(
                12,
                Size {
                    width: 1920,
                    height: 1080,
                },
            ),
            Grid {
                columns: 4,
                rows: 3,
            }
        );
    }

    #[test]
    fn auto_canvas_tile_count_follows_zoom_distance() {
        let output = Size {
            width: 1920,
            height: 1080,
        };

        assert_eq!(auto_canvas_tile_count(1.0 / 3.0, output, 64), 15);
        assert_eq!(auto_canvas_tile_count(0.25, output, 64), 35);
        assert_eq!(auto_canvas_tile_count(0.25, output, 16), 16);
        assert_eq!(auto_canvas_tile_count_for_pan(0.25, output, 64, 3), 55);
    }

    #[test]
    fn overview_grid_keeps_scale_depth_for_landscape() {
        let output = Size {
            width: 1920,
            height: 1080,
        };

        assert_eq!(
            canvas_grid_for_overview(132, output, 0.1),
            Grid {
                columns: 12,
                rows: 11,
            }
        );
    }

    #[test]
    fn overview_grid_can_force_horizontal_axis_on_portrait() {
        let output = Size {
            width: 1080,
            height: 1920,
        };

        assert_eq!(
            canvas_grid_for_overview_axis(55, output, 0.25, true),
            Grid {
                columns: 11,
                rows: 5,
            }
        );
        assert_eq!(
            canvas_grid_for_overview_axis(55, output, 0.25, false),
            Grid {
                columns: 5,
                rows: 11,
            }
        );
    }

    #[test]
    fn canvas_transform_centers_current_then_pans_to_target() {
        let grid = Grid {
            columns: 4,
            rows: 3,
        };
        let start = canvas_transform(grid, 5, 10, 0.0, 1.0 / 3.0, 0.35, 0.15, Easing::Linear);
        assert_near(start.scale, 1.0);
        assert_near(start.translate_x, -1.0);
        assert_near(start.translate_y, -1.0);

        let old_centered_overview =
            canvas_transform(grid, 5, 10, 0.35, 1.0 / 3.0, 0.35, 0.15, Easing::Linear);
        assert_near(old_centered_overview.scale, 1.0 / 3.0);
        assert_near(old_centered_overview.translate_x, 0.0);
        assert_near(old_centered_overview.translate_y, 0.0);

        let target_centered_overview =
            canvas_transform(grid, 5, 10, 0.5, 1.0 / 3.0, 0.35, 0.15, Easing::Linear);
        assert_near(target_centered_overview.scale, 1.0 / 3.0);
        assert_near(target_centered_overview.translate_x, -1.0 / 3.0);
        assert_near(target_centered_overview.translate_y, -1.0 / 3.0);

        let end = canvas_transform(grid, 5, 10, 1.0, 1.0 / 3.0, 0.35, 0.15, Easing::Linear);
        assert_near(end.scale, 1.0);
        assert_near(end.translate_x, -2.0);
        assert_near(end.translate_y, -2.0);
    }
}
