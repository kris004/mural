use mural_ipc::{CanvasPanAxis, CanvasTileCount};
use mural_render::Size;

use crate::MuralApp;
use crate::surface::surface_size;
use crate::transitions::canvas::{CanvasSpan, CanvasSpanSlot, resolve_canvas_tile_count_for_pan};

impl MuralApp {
    pub(crate) fn resolve_canvas_tile_count(
        &self,
        tile_count: CanvasTileCount,
        overview_scale: f32,
        pan_axis: CanvasPanAxis,
    ) -> usize {
        let output = self.canvas_reference_output_size();
        resolve_canvas_tile_count_for_pan(
            tile_count,
            overview_scale,
            output,
            self.canvas_pan_tile_distance(),
            pan_axis,
        )
    }

    fn canvas_reference_output_size(&self) -> Size {
        self.surfaces
            .iter()
            .filter_map(surface_size)
            .max_by_key(|size| u64::from(size.width) * u64::from(size.height))
            .unwrap_or(Size {
                width: 1920,
                height: 1080,
            })
    }

    pub(crate) fn canvas_pan_tile_distance(&self) -> usize {
        self.surfaces.len().max(1)
    }

    pub(crate) fn canvas_span_for_surface(&self, surface_index: usize) -> Option<CanvasSpan> {
        let surface = self.surfaces.get(surface_index)?;
        if surface.width <= 0 || surface.height <= 0 {
            return None;
        }

        let mut ordered = self
            .surfaces
            .iter()
            .enumerate()
            .filter(|(_, surface)| surface.width > 0 && surface.height > 0)
            .collect::<Vec<_>>();
        ordered.sort_by(|(_, left), (_, right)| {
            (left.layout_x, left.layout_y, left.name.as_str()).cmp(&(
                right.layout_x,
                right.layout_y,
                right.name.as_str(),
            ))
        });
        let output_index = ordered
            .iter()
            .position(|(index, _)| *index == surface_index)
            .unwrap_or(0);
        let output_count = ordered.len().max(1);

        let mut min_x = surface.layout_x;
        let mut min_y = surface.layout_y;
        let mut max_x = surface.layout_x.saturating_add(surface.width.max(1));
        let mut max_y = surface.layout_y.saturating_add(surface.height.max(1));
        for (_, surface) in &ordered {
            min_x = min_x.min(surface.layout_x);
            min_y = min_y.min(surface.layout_y);
            max_x = max_x.max(surface.layout_x.saturating_add(surface.width.max(1)));
            max_y = max_y.max(surface.layout_y.saturating_add(surface.height.max(1)));
        }

        let desktop_width = max_x.saturating_sub(min_x).max(1);
        let desktop_height = max_y.saturating_sub(min_y).max(1);
        let slots = ordered
            .iter()
            .map(|(_, surface)| CanvasSpanSlot {
                x: surface.layout_x.saturating_sub(min_x),
                y: surface.layout_y.saturating_sub(min_y),
                width: surface.width.max(1),
                height: surface.height.max(1),
            })
            .collect();
        Some(CanvasSpan {
            desktop_width,
            desktop_height,
            viewport_x: surface.layout_x.saturating_sub(min_x),
            viewport_y: surface.layout_y.saturating_sub(min_y),
            viewport_width: surface.width.max(1),
            viewport_height: surface.height.max(1),
            output_index,
            output_count,
            slots,
        })
    }
}
