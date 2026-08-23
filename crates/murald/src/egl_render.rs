use std::cell::OnceCell;
use std::ffi::c_void;
use std::mem::size_of;

use glow::HasContext as _;
use khronos_egl as egl;
use mural_core::world_cache::world_lod_tile_cells;
use mural_ipc::{CanvasMode, CanvasPanAxis, PushMode, ScaleMode, WorldRouteFocus};
use mural_render::{
    Easing as RenderEasing, Offset, PushDirection as RenderPushDirection, Rect, Size, WorldLayout,
    canvas_grid_for_overview_axis, canvas_transform, eased_progress, fade_weights, push_offsets,
    world_camera_path, world_camera_view, world_cell_rect,
};
use wayland_client::Connection;
use wayland_egl::WlEglSurface;

use crate::decode::DecodedImage;
use crate::transitions::canvas::{
    CanvasLayoutInput, CanvasModeTransform, CanvasRectLayout, CanvasSpan, CanvasTile,
    canvas_collage_layout, canvas_final_transform, canvas_final_transform_for_rect,
    canvas_full_thumbnail_draw_order, canvas_mode_transform, canvas_morph_layout,
    canvas_morph_rects, canvas_overlap_layout, canvas_pan_axis_is_horizontal,
    canvas_phase_fractions, canvas_rect_apply_transform, canvas_span_draw_order,
    canvas_span_group_rect, canvas_span_morph_rect, canvas_walk_axis,
    centered_canvas_overview_transform,
};
#[cfg(test)]
use crate::transitions::canvas::{
    CanvasPoint, CanvasSpanSlot, canvas_collage_bounds, canvas_collage_rects, canvas_gap_rects,
    canvas_horizontal_morph_rects, canvas_morph_pack_axis, canvas_overlap_rects,
    canvas_rect_center, canvas_span_focus_slot_rect, canvas_span_slot_rect,
    canvas_vertical_morph_rects, expand_canvas_focus_layout,
};
use crate::transitions::world::WorldTileTexture;
#[cfg(test)]
use mural_render::Grid;

type EglMakeCurrent = unsafe extern "system" fn(
    egl::EGLDisplay,
    egl::EGLSurface,
    egl::EGLSurface,
    egl::EGLContext,
) -> egl::Boolean;
type EglSwapBuffers = unsafe extern "system" fn(egl::EGLDisplay, egl::EGLSurface) -> egl::Boolean;
type EglDestroySurface =
    unsafe extern "system" fn(egl::EGLDisplay, egl::EGLSurface) -> egl::Boolean;

#[derive(Clone, Copy)]
pub(crate) struct WallpaperTexture {
    texture: glow::NativeTexture,
    width: i32,
    height: i32,
}

#[derive(Clone)]
pub(crate) struct CanvasFrame<'a> {
    pub(crate) old: Option<WallpaperTexture>,
    pub(crate) old_scale_mode: ScaleMode,
    pub(crate) new: Option<WallpaperTexture>,
    pub(crate) new_scale_mode: ScaleMode,
    pub(crate) clear_color: Color,
    pub(crate) easing: mural_ipc::Easing,
    pub(crate) tiles: &'a [CanvasTile],
    pub(crate) old_index: usize,
    pub(crate) target_index: usize,
    pub(crate) zoom_out_ms: u64,
    pub(crate) pan_ms: u64,
    pub(crate) zoom_in_ms: u64,
    pub(crate) mode: CanvasMode,
    pub(crate) pan_axis: CanvasPanAxis,
    pub(crate) overview_scale: f32,
    pub(crate) span: Option<CanvasSpan>,
    pub(crate) progress: f32,
}

pub(crate) struct WorldFrame<'a> {
    pub(crate) old: Option<WallpaperTexture>,
    pub(crate) old_scale_mode: ScaleMode,
    pub(crate) new: WallpaperTexture,
    pub(crate) new_scale_mode: ScaleMode,
    pub(crate) clear_color: Color,
    pub(crate) easing: mural_ipc::Easing,
    pub(crate) library_count: usize,
    pub(crate) columns: usize,
    pub(crate) tile_cells: usize,
    pub(crate) route: WorldRouteFocus,
    pub(crate) tiles: &'a [WorldTileTexture],
    pub(crate) progress: f32,
}

#[derive(Clone, Copy)]
struct CanvasTileDraw {
    wallpaper: WallpaperTexture,
    scale_mode: ScaleMode,
    rect: WallpaperRect,
}

#[derive(Clone, Copy)]
pub(crate) struct PushFrame {
    pub(crate) old: Option<WallpaperTexture>,
    pub(crate) old_scale_mode: ScaleMode,
    pub(crate) new: WallpaperTexture,
    pub(crate) new_scale_mode: ScaleMode,
    pub(crate) clear_color: Color,
    pub(crate) direction: mural_ipc::PushDirection,
    pub(crate) easing: mural_ipc::Easing,
    pub(crate) mode: PushMode,
    pub(crate) progress: f32,
}

#[derive(Clone, Copy)]
pub(crate) struct FadeFrame {
    pub(crate) old: Option<WallpaperTexture>,
    pub(crate) old_scale_mode: ScaleMode,
    pub(crate) new: WallpaperTexture,
    pub(crate) new_scale_mode: ScaleMode,
    pub(crate) clear_color: Color,
    pub(crate) easing: mural_ipc::Easing,
    pub(crate) progress: f32,
}

#[derive(Clone, Copy)]
struct PortalDraw {
    output_width: i32,
    output_height: i32,
    wallpaper: WallpaperTexture,
    scale_mode: ScaleMode,
    direction: mural_ipc::PushDirection,
    progress: f32,
    image: PortalImage,
    offset: Offset,
    pan: bool,
}

#[derive(Clone, Copy)]
struct PortalOffsets {
    output_width: i32,
    output_height: i32,
    old: WallpaperTexture,
    old_scale_mode: ScaleMode,
    new: WallpaperTexture,
    new_scale_mode: ScaleMode,
    direction: mural_ipc::PushDirection,
    progress: f32,
}

#[derive(Clone, Copy)]
struct PortalImageDraw<'a> {
    frame: &'a PushFrame,
    wallpaper: WallpaperTexture,
    image: PortalImage,
    offset: Offset,
    pan: bool,
    progress: f32,
}

#[derive(Clone, Copy)]
enum PortalImage {
    Old,
    New,
}

#[derive(Clone, Copy)]
struct ScissorRect {
    x: i32,
    y: i32,
    width: i32,
    height: i32,
}

pub(crate) struct EglState {
    pub(crate) api: egl::DynamicInstance<egl::EGL1_5>,
    pub(crate) display: egl::Display,
    config: egl::Config,
    context: egl::Context,
    egl_make_current: EglMakeCurrent,
    egl_swap_buffers: EglSwapBuffers,
    egl_destroy_surface: EglDestroySurface,
    gl: OnceCell<glow::Context>,
    renderer: OnceCell<GlRenderer>,
    fade_renderer: OnceCell<FadeRenderer>,
}

impl EglState {
    pub(crate) fn new(conn: &Connection) -> Result<Self, String> {
        if !wayland_egl::is_available() {
            return Err("libwayland-egl is not available".to_owned());
        }

        let api = unsafe { egl::DynamicInstance::<egl::EGL1_5>::load_required() }
            .map_err(|error| format!("failed to load libEGL: {error}"))?;
        api.bind_api(egl::OPENGL_ES_API)
            .map_err(|error| format!("failed to bind OpenGL ES API: {error}"))?;

        let display = unsafe { api.get_display(conn.backend().display_ptr().cast::<c_void>()) }
            .ok_or_else(|| "eglGetDisplay returned no display".to_owned())?;
        api.initialize(display)
            .map_err(|error| format!("eglInitialize failed: {error}"))?;

        let config_attributes = [
            egl::SURFACE_TYPE,
            egl::WINDOW_BIT,
            egl::RENDERABLE_TYPE,
            egl::OPENGL_ES2_BIT,
            egl::RED_SIZE,
            8,
            egl::GREEN_SIZE,
            8,
            egl::BLUE_SIZE,
            8,
            egl::ALPHA_SIZE,
            8,
            egl::NONE,
        ];
        let config = api
            .choose_first_config(display, &config_attributes)
            .map_err(|error| format!("eglChooseConfig failed: {error}"))?
            .ok_or_else(|| "no suitable EGL window config found".to_owned())?;

        let context_attributes = [egl::CONTEXT_CLIENT_VERSION, 2, egl::NONE];
        let context = api
            .create_context(display, config, None, &context_attributes)
            .map_err(|error| format!("eglCreateContext failed: {error}"))?;
        let egl_make_current = load_egl_make_current(&api)?;
        let egl_swap_buffers = load_egl_swap_buffers(&api)?;
        let egl_destroy_surface = load_egl_destroy_surface(&api)?;

        Ok(Self {
            api,
            display,
            config,
            context,
            egl_make_current,
            egl_swap_buffers,
            egl_destroy_surface,
            gl: OnceCell::new(),
            renderer: OnceCell::new(),
            fade_renderer: OnceCell::new(),
        })
    }

    pub(crate) fn create_window_surface(
        &self,
        egl_window: &WlEglSurface,
    ) -> Result<egl::Surface, String> {
        unsafe {
            self.api.create_window_surface(
                self.display,
                self.config,
                egl_window.ptr().cast_mut(),
                None,
            )
        }
        .map_err(|error| format!("eglCreateWindowSurface failed: {error}"))
    }

    pub(crate) fn configure_swap_interval(&self, surface: egl::Surface) -> Result<(), String> {
        self.make_current(surface)?;
        // Mesa's Wayland EGL path uses frame/sync callbacks to throttle
        // eglSwapBuffers() at the default swap interval. If a callback is
        // stranded across DPMS/suspend, swap can block the daemon event loop.
        // murald drives animations with its own wl_surface.frame callbacks, so
        // prefer interval 0 while tolerating drivers that mandate throttling.
        if self
            .api
            .get_config_attrib(self.display, self.config, egl::MIN_SWAP_INTERVAL)
            .is_ok_and(|minimum| minimum > 0)
        {
            eprintln!(
                "murald: EGL driver does not support swap interval 0; using driver throttling"
            );
            return Ok(());
        }

        if let Err(error) = self.api.swap_interval(self.display, 0) {
            eprintln!(
                "murald: eglSwapInterval(0) is unavailable ({error}); using driver throttling"
            );
        }
        Ok(())
    }

    pub(crate) fn swap_buffers(&self, surface: egl::Surface) -> Result<(), String> {
        let result = unsafe { (self.egl_swap_buffers)(self.display.as_ptr(), surface.as_ptr()) };
        self.egl_boolean_result("eglSwapBuffers", result)
    }

    pub(crate) fn destroy_surface(&self, surface: egl::Surface) -> Result<(), String> {
        let result = unsafe { (self.egl_destroy_surface)(self.display.as_ptr(), surface.as_ptr()) };
        self.egl_boolean_result("eglDestroySurface", result)
    }

    pub(crate) fn render_clear(
        &self,
        surface: egl::Surface,
        width: i32,
        height: i32,
        color: Color,
    ) -> Result<(), String> {
        self.make_current(surface)?;
        let gl = self.gl()?;
        unsafe {
            gl.viewport(0, 0, width, height);
            gl.clear_color(color.r, color.g, color.b, color.a);
            gl.clear(glow::COLOR_BUFFER_BIT);
        }

        Ok(())
    }

    pub(crate) fn render_wallpaper(
        &self,
        surface: egl::Surface,
        width: i32,
        height: i32,
        wallpaper: WallpaperTexture,
        scale_mode: ScaleMode,
        clear_color: Color,
    ) -> Result<(), String> {
        self.make_current(surface)?;
        let gl = self.gl()?;
        unsafe {
            gl.viewport(0, 0, width, height);
            gl.clear_color(clear_color.r, clear_color.g, clear_color.b, clear_color.a);
            gl.clear(glow::COLOR_BUFFER_BIT);
        }
        self.renderer()?
            .draw(gl, width, height, wallpaper, scale_mode);
        Ok(())
    }

    pub(crate) fn render_push(
        &self,
        surface: egl::Surface,
        width: i32,
        height: i32,
        frame: PushFrame,
    ) -> Result<(), String> {
        self.make_current(surface)?;
        let gl = self.gl()?;
        clear_gl(gl, width, height, frame.clear_color);

        let render_easing = render_easing(frame.easing);
        let offsets = push_offsets(
            render_push_direction(frame.direction),
            frame.progress,
            render_easing,
        );
        let eased_progress = eased_progress(frame.progress, render_easing);
        let renderer = self.renderer()?;
        match frame.mode {
            PushMode::Screen => {
                render_screen_push(
                    renderer,
                    gl,
                    width,
                    height,
                    &frame,
                    offsets.old,
                    offsets.new,
                );
            }
            PushMode::Portal => {
                render_portal_push_frame(
                    renderer,
                    gl,
                    width,
                    height,
                    &frame,
                    offsets.new,
                    eased_progress,
                );
            }
            PushMode::Pan => {
                render_pan_push_frame(renderer, gl, width, height, &frame, eased_progress);
            }
        }
        Ok(())
    }

    pub(crate) fn render_fade(
        &self,
        surface: egl::Surface,
        width: i32,
        height: i32,
        frame: FadeFrame,
    ) -> Result<(), String> {
        self.make_current(surface)?;
        let gl = self.gl()?;
        clear_gl(gl, width, height, frame.clear_color);
        self.fade_renderer()?.draw(
            gl,
            width,
            height,
            frame,
            fade_weights(frame.progress, render_easing(frame.easing)).new,
        );
        Ok(())
    }

    pub(crate) fn render_canvas(
        &self,
        surface: egl::Surface,
        width: i32,
        height: i32,
        frame: &CanvasFrame<'_>,
    ) -> Result<(), String> {
        self.make_current(surface)?;
        let gl = self.gl()?;
        unsafe {
            gl.viewport(0, 0, width, height);
            gl.clear_color(
                frame.clear_color.r,
                frame.clear_color.g,
                frame.clear_color.b,
                frame.clear_color.a,
            );
            gl.clear(glow::COLOR_BUFFER_BIT);
        }

        if frame.tiles.is_empty() || width <= 0 || height <= 0 {
            return Ok(());
        }

        let output = Size {
            width: u32::try_from(width).map_err(|_| "output width is negative".to_owned())?,
            height: u32::try_from(height).map_err(|_| "output height is negative".to_owned())?,
        };
        match frame.mode {
            CanvasMode::Clipped => self.render_canvas_clipped(gl, width, height, output, frame),
            CanvasMode::Morph => {
                let aspects = canvas_mode_aspects(frame, output);
                self.render_canvas_rect_layout(
                    gl,
                    width,
                    height,
                    frame,
                    canvas_morph_layout(canvas_layout_input(frame, output, &aspects)),
                )
            }
            CanvasMode::Overlap => {
                let aspects = canvas_mode_aspects(frame, output);
                self.render_canvas_rect_layout(
                    gl,
                    width,
                    height,
                    frame,
                    canvas_overlap_layout(canvas_layout_input(frame, output, &aspects)),
                )
            }
            CanvasMode::Collage => {
                let aspects = canvas_mode_aspects(frame, output);
                self.render_canvas_rect_layout(
                    gl,
                    width,
                    height,
                    frame,
                    canvas_collage_layout(canvas_layout_input(frame, output, &aspects)),
                )
            }
            CanvasMode::Span => {
                if let Some(span) = frame.span.as_ref() {
                    self.render_canvas_span(gl, width, height, output, frame, span)
                } else {
                    let aspects = canvas_mode_aspects(frame, output);
                    self.render_canvas_rect_layout(
                        gl,
                        width,
                        height,
                        frame,
                        canvas_morph_layout(canvas_layout_input(frame, output, &aspects)),
                    )
                }
            }
        }
    }

    pub(crate) fn render_world(
        &self,
        surface: egl::Surface,
        width: i32,
        height: i32,
        frame: &WorldFrame<'_>,
    ) -> Result<(), String> {
        self.make_current(surface)?;
        let gl = self.gl()?;
        clear_gl(gl, width, height, frame.clear_color);

        if frame.tiles.is_empty() || width <= 0 || height <= 0 {
            return Ok(());
        }

        let layout = WorldLayout::new(frame.library_count, frame.columns);
        let path = world_camera_path(
            layout,
            frame.route.current_index,
            frame.route.target_index,
            1.0,
        )
        .ok_or_else(|| "world transition route indices are outside the world layout".to_owned())?;
        let view = world_camera_view(path, frame.progress, render_easing(frame.easing));
        let renderer = self.renderer()?;

        unsafe {
            gl.enable(glow::SCISSOR_TEST);
        }
        for tile in frame.tiles {
            let rect = world_tile_rect_for_view(tile, frame.tile_cells, view, width, height);
            set_scissor(gl, canvas_tile_scissor(width, height, rect));
            renderer.draw_in_rect(
                gl,
                width,
                height,
                CanvasTileDraw {
                    wallpaper: tile.texture,
                    scale_mode: ScaleMode::Stretch,
                    rect,
                },
            );
        }

        if let Some(old) = frame.old
            && let Some(rect) =
                world_cell_draw_rect(layout, frame.route.current_index, view, width, height)
        {
            set_scissor(gl, canvas_tile_scissor(width, height, rect));
            renderer.draw_in_rect(
                gl,
                width,
                height,
                CanvasTileDraw {
                    wallpaper: old,
                    scale_mode: frame.old_scale_mode,
                    rect,
                },
            );
        }
        if let Some(rect) =
            world_cell_draw_rect(layout, frame.route.target_index, view, width, height)
        {
            set_scissor(gl, canvas_tile_scissor(width, height, rect));
            renderer.draw_in_rect(
                gl,
                width,
                height,
                CanvasTileDraw {
                    wallpaper: frame.new,
                    scale_mode: frame.new_scale_mode,
                    rect,
                },
            );
        }
        unsafe {
            gl.disable(glow::SCISSOR_TEST);
        }
        Ok(())
    }

    fn render_canvas_clipped(
        &self,
        gl: &glow::Context,
        width: i32,
        height: i32,
        output: Size,
        frame: &CanvasFrame<'_>,
    ) -> Result<(), String> {
        let walk_axis = canvas_walk_axis(frame.pan_axis, output);
        let grid = canvas_grid_for_overview_axis(
            frame.tiles.len(),
            output,
            frame.overview_scale,
            walk_axis.is_horizontal(),
        );
        let phase = canvas_phase_fractions(frame.zoom_out_ms, frame.pan_ms, frame.zoom_in_ms);
        let transform = canvas_transform(
            grid,
            frame.old_index.min(frame.tiles.len() - 1),
            frame.target_index.min(frame.tiles.len() - 1),
            frame.progress,
            frame.overview_scale,
            phase.zoom_out,
            phase.pan,
            render_easing(frame.easing),
        );
        let renderer = self.renderer()?;

        #[allow(clippy::cast_precision_loss)]
        let (output_width, output_height) = (width as f32, height as f32);
        for (index, tile) in frame.tiles.iter().enumerate() {
            let Some((wallpaper, scale_mode)) = canvas_tile_texture(frame, index, tile) else {
                continue;
            };
            let column = index % grid.columns.max(1);
            let row = index / grid.columns.max(1);
            #[allow(clippy::cast_precision_loss)]
            let rect = WallpaperRect {
                x: (column as f32 * transform.scale + transform.translate_x) * output_width,
                y: (row as f32 * transform.scale + transform.translate_y) * output_height,
                width: transform.scale * output_width,
                height: transform.scale * output_height,
            };
            unsafe {
                gl.enable(glow::SCISSOR_TEST);
            }
            set_scissor(gl, canvas_tile_scissor(width, height, rect));
            renderer.draw_in_rect(
                gl,
                width,
                height,
                CanvasTileDraw {
                    wallpaper,
                    scale_mode,
                    rect,
                },
            );
        }
        unsafe {
            gl.disable(glow::SCISSOR_TEST);
        }

        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    fn render_canvas_rect_layout(
        &self,
        gl: &glow::Context,
        width: i32,
        height: i32,
        frame: &CanvasFrame<'_>,
        layout: Option<CanvasRectLayout>,
    ) -> Result<(), String> {
        let Some(layout) = layout else {
            return Ok(());
        };
        let old_rect = layout.rects[layout.old_index];
        let target_rect = layout.rects[layout.target_index];
        let (old_final, old_overview, target_overview, target_final) =
            if layout.raster.full_thumbnail() {
                (
                    canvas_full_thumbnail_final_transform(
                        frame,
                        layout.old_index,
                        &layout.rects,
                        true,
                        width,
                        height,
                    ),
                    centered_canvas_overview_transform(old_rect, frame.overview_scale),
                    centered_canvas_overview_transform(target_rect, frame.overview_scale),
                    canvas_full_thumbnail_final_transform(
                        frame,
                        layout.target_index,
                        &layout.rects,
                        false,
                        width,
                        height,
                    ),
                )
            } else {
                (
                    canvas_final_transform_for_rect(old_rect),
                    centered_canvas_overview_transform(old_rect, frame.overview_scale),
                    centered_canvas_overview_transform(target_rect, frame.overview_scale),
                    canvas_final_transform_for_rect(target_rect),
                )
            };
        let phase = canvas_phase_fractions(frame.zoom_out_ms, frame.pan_ms, frame.zoom_in_ms);
        let transform = canvas_mode_transform(
            old_final,
            old_overview,
            target_overview,
            target_final,
            frame.progress,
            phase.zoom_out,
            phase.pan,
            render_easing(frame.easing),
        );
        let renderer = self.renderer()?;

        #[allow(clippy::cast_precision_loss)]
        let (output_width, output_height) = (width as f32, height as f32);
        let draw_order = if layout.raster.full_thumbnail() {
            canvas_full_thumbnail_draw_order(
                frame.tiles.len(),
                layout.old_index,
                layout.target_index,
                frame.progress,
                phase.zoom_out,
                phase.pan,
            )
        } else {
            (0..frame.tiles.len()).collect()
        };
        for index in draw_order {
            let Some(tile) = frame.tiles.get(index) else {
                continue;
            };
            let Some((wallpaper, scale_mode)) = canvas_tile_texture(frame, index, tile) else {
                continue;
            };
            let scale_mode = if layout.raster.full_thumbnail() {
                ScaleMode::Stretch
            } else {
                scale_mode
            };
            let Some(rect) = layout.rects.get(index).copied() else {
                continue;
            };
            let rect =
                canvas_draw_rect_for_view(rect, transform, None, output_width, output_height);
            unsafe {
                gl.enable(glow::SCISSOR_TEST);
            }
            set_scissor(gl, canvas_tile_scissor(width, height, rect));
            renderer.draw_in_rect(
                gl,
                width,
                height,
                CanvasTileDraw {
                    wallpaper,
                    scale_mode,
                    rect,
                },
            );
        }
        unsafe {
            gl.disable(glow::SCISSOR_TEST);
        }

        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    fn render_canvas_span(
        &self,
        gl: &glow::Context,
        width: i32,
        height: i32,
        _output: Size,
        frame: &CanvasFrame<'_>,
        span: &CanvasSpan,
    ) -> Result<(), String> {
        let layout_output = span.output_size();
        let horizontal = canvas_pan_axis_is_horizontal(frame.pan_axis, layout_output);
        let grid = canvas_grid_for_overview_axis(
            frame.tiles.len(),
            layout_output,
            frame.overview_scale,
            horizontal,
        );
        let aspects = canvas_mode_aspects(frame, layout_output);
        let rects = canvas_morph_rects(grid, &aspects, horizontal);
        if rects.is_empty() {
            return Ok(());
        }

        let old_index = frame.old_index.min(rects.len() - 1);
        let target_index = frame.target_index.min(rects.len() - 1);
        let old_rect = canvas_span_group_rect(old_index, &rects, span);
        let target_rect = canvas_span_group_rect(target_index, &rects, span);
        let old_final = canvas_final_transform_for_rect(old_rect);
        let old_overview = centered_canvas_overview_transform(old_rect, frame.overview_scale);
        let target_overview = centered_canvas_overview_transform(target_rect, frame.overview_scale);
        let target_final = canvas_final_transform_for_rect(target_rect);
        let phase = canvas_phase_fractions(frame.zoom_out_ms, frame.pan_ms, frame.zoom_in_ms);
        let easing = render_easing(frame.easing);
        let transform = canvas_mode_transform(
            old_final,
            old_overview,
            target_overview,
            target_final,
            frame.progress,
            phase.zoom_out,
            phase.pan,
            easing,
        );
        let renderer = self.renderer()?;

        #[allow(clippy::cast_precision_loss)]
        let (output_width, output_height) = (width as f32, height as f32);
        for index in canvas_span_draw_order(
            frame.tiles.len(),
            old_index,
            target_index,
            span,
            frame.progress,
            phase.zoom_out,
            phase.pan,
        ) {
            let Some(tile) = frame.tiles.get(index) else {
                continue;
            };
            let Some((wallpaper, scale_mode)) = canvas_tile_texture(frame, index, tile) else {
                continue;
            };
            let Some(rect) = rects.get(index).copied() else {
                continue;
            };
            let rect = canvas_span_morph_rect(
                index,
                rect,
                old_index,
                target_index,
                &rects,
                span,
                frame.progress,
                phase.zoom_out,
                phase.pan,
                easing,
            );
            let rect =
                canvas_draw_rect_for_view(rect, transform, Some(span), output_width, output_height);
            unsafe {
                gl.enable(glow::SCISSOR_TEST);
            }
            set_scissor(gl, canvas_tile_scissor(width, height, rect));
            renderer.draw_in_rect(
                gl,
                width,
                height,
                CanvasTileDraw {
                    wallpaper,
                    scale_mode,
                    rect,
                },
            );
        }
        unsafe {
            gl.disable(glow::SCISSOR_TEST);
        }

        Ok(())
    }

    pub(crate) fn upload_texture(
        &self,
        surface: egl::Surface,
        image: &DecodedImage,
    ) -> Result<WallpaperTexture, String> {
        self.make_current(surface)?;
        let gl = self.gl()?;

        let texture = unsafe {
            let texture = gl
                .create_texture()
                .map_err(|error| format!("failed to create GL texture: {error}"))?;
            gl.bind_texture(glow::TEXTURE_2D, Some(texture));
            gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 1);
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_MIN_FILTER,
                gl_i32(glow::LINEAR),
            );
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_MAG_FILTER,
                gl_i32(glow::LINEAR),
            );
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_WRAP_S,
                gl_i32(glow::CLAMP_TO_EDGE),
            );
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_WRAP_T,
                gl_i32(glow::CLAMP_TO_EDGE),
            );
            gl.tex_image_2d(
                glow::TEXTURE_2D,
                0,
                gl_i32(glow::RGBA),
                image.width,
                image.height,
                0,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelUnpackData::Slice(Some(&image.pixels)),
            );
            gl.bind_texture(glow::TEXTURE_2D, None);
            texture
        };

        Ok(WallpaperTexture {
            texture,
            width: image.width,
            height: image.height,
        })
    }

    pub(crate) fn delete_texture(&self, texture: WallpaperTexture) {
        if let Some(gl) = self.gl.get() {
            unsafe {
                gl.delete_texture(texture.texture);
            }
        }
    }

    fn make_current(&self, surface: egl::Surface) -> Result<(), String> {
        let result = unsafe {
            (self.egl_make_current)(
                self.display.as_ptr(),
                surface.as_ptr(),
                surface.as_ptr(),
                self.context.as_ptr(),
            )
        };
        self.egl_boolean_result("eglMakeCurrent", result)
    }

    fn egl_boolean_result(&self, operation: &str, result: egl::Boolean) -> Result<(), String> {
        if result == egl::TRUE {
            return Ok(());
        }

        let error = self.api.get_error().map_or_else(
            || "no EGL error reported".to_owned(),
            |error| error.to_string(),
        );
        Err(format!("{operation} failed: {error}"))
    }

    fn gl(&self) -> Result<&glow::Context, String> {
        if self.gl.get().is_none() {
            let gl = unsafe {
                glow::Context::from_loader_function(|name| {
                    self.api
                        .get_proc_address(name)
                        .map_or(std::ptr::null(), |function| {
                            (function as *const ()).cast::<c_void>()
                        })
                })
            };
            self.gl
                .set(gl)
                .map_err(|_| "failed to initialize GL context".to_owned())?;
        }

        self.gl
            .get()
            .ok_or_else(|| "GL context is not initialized".to_owned())
    }

    fn renderer(&self) -> Result<&GlRenderer, String> {
        if self.renderer.get().is_none() {
            let renderer = GlRenderer::new(self.gl()?)?;
            self.renderer
                .set(renderer)
                .map_err(|_| "failed to initialize GL renderer".to_owned())?;
        }

        self.renderer
            .get()
            .ok_or_else(|| "GL renderer is not initialized".to_owned())
    }

    fn fade_renderer(&self) -> Result<&FadeRenderer, String> {
        if self.fade_renderer.get().is_none() {
            let renderer = FadeRenderer::new(self.gl()?)?;
            self.fade_renderer
                .set(renderer)
                .map_err(|_| "failed to initialize fade renderer".to_owned())?;
        }

        self.fade_renderer
            .get()
            .ok_or_else(|| "fade renderer is not initialized".to_owned())
    }
}

fn load_egl_make_current(
    api: &egl::DynamicInstance<egl::EGL1_5>,
) -> Result<EglMakeCurrent, String> {
    let Some(function) = api.get_proc_address("eglMakeCurrent") else {
        return Err("failed to load EGL function eglMakeCurrent".to_owned());
    };
    // SAFETY: eglGetProcAddress returned the named EGL entry point; the cast
    // gives it the EGL 1.0 eglMakeCurrent signature.
    Ok(unsafe { std::mem::transmute::<extern "system" fn(), EglMakeCurrent>(function) })
}

fn load_egl_swap_buffers(
    api: &egl::DynamicInstance<egl::EGL1_5>,
) -> Result<EglSwapBuffers, String> {
    let Some(function) = api.get_proc_address("eglSwapBuffers") else {
        return Err("failed to load EGL function eglSwapBuffers".to_owned());
    };
    // SAFETY: eglGetProcAddress returned the named EGL entry point; the cast
    // gives it the EGL 1.0 eglSwapBuffers signature.
    Ok(unsafe { std::mem::transmute::<extern "system" fn(), EglSwapBuffers>(function) })
}

fn load_egl_destroy_surface(
    api: &egl::DynamicInstance<egl::EGL1_5>,
) -> Result<EglDestroySurface, String> {
    let Some(function) = api.get_proc_address("eglDestroySurface") else {
        return Err("failed to load EGL function eglDestroySurface".to_owned());
    };
    // SAFETY: eglGetProcAddress returned the named EGL entry point; the cast
    // gives it the EGL 1.0 eglDestroySurface signature.
    Ok(unsafe { std::mem::transmute::<extern "system" fn(), EglDestroySurface>(function) })
}

struct GlRenderer {
    program: glow::NativeProgram,
    vertex_buffer: glow::NativeBuffer,
    position_attribute: u32,
    texcoord_attribute: u32,
    texture_uniform: Option<glow::NativeUniformLocation>,
}

impl GlRenderer {
    fn new(gl: &glow::Context) -> Result<Self, String> {
        let program = unsafe {
            let vertex_shader = compile_shader(gl, glow::VERTEX_SHADER, VERTEX_SHADER_SOURCE)?;
            let fragment_shader =
                compile_shader(gl, glow::FRAGMENT_SHADER, FRAGMENT_SHADER_SOURCE)?;
            let program = gl
                .create_program()
                .map_err(|error| format!("failed to create GL program: {error}"))?;
            gl.attach_shader(program, vertex_shader);
            gl.attach_shader(program, fragment_shader);
            gl.link_program(program);

            gl.detach_shader(program, vertex_shader);
            gl.detach_shader(program, fragment_shader);
            gl.delete_shader(vertex_shader);
            gl.delete_shader(fragment_shader);

            if !gl.get_program_link_status(program) {
                let log = gl.get_program_info_log(program);
                gl.delete_program(program);
                return Err(format!("failed to link wallpaper shader program: {log}"));
            }

            program
        };

        let vertex_buffer = unsafe {
            gl.create_buffer()
                .map_err(|error| format!("failed to create GL vertex buffer: {error}"))?
        };
        let position_attribute = unsafe {
            gl.get_attrib_location(program, "a_position")
                .ok_or_else(|| "shader attribute a_position is missing".to_owned())?
        };
        let texcoord_attribute = unsafe {
            gl.get_attrib_location(program, "a_texcoord")
                .ok_or_else(|| "shader attribute a_texcoord is missing".to_owned())?
        };
        let texture_uniform = unsafe { gl.get_uniform_location(program, "u_texture") };

        Ok(Self {
            program,
            vertex_buffer,
            position_attribute,
            texcoord_attribute,
            texture_uniform,
        })
    }

    fn draw(
        &self,
        gl: &glow::Context,
        output_width: i32,
        output_height: i32,
        wallpaper: WallpaperTexture,
        scale_mode: ScaleMode,
    ) {
        self.draw_with_offset(
            gl,
            output_width,
            output_height,
            wallpaper,
            scale_mode,
            Offset { x: 0.0, y: 0.0 },
        );
    }

    fn draw_with_offset(
        &self,
        gl: &glow::Context,
        output_width: i32,
        output_height: i32,
        wallpaper: WallpaperTexture,
        scale_mode: ScaleMode,
        offset: Offset,
    ) {
        let vertices =
            wallpaper_vertices(output_width, output_height, wallpaper, scale_mode, offset);
        self.draw_vertices(gl, wallpaper, vertices);
    }

    fn draw_portal_push(&self, gl: &glow::Context, draw: PortalDraw) {
        let vertices = portal_wallpaper_vertices(draw);
        self.draw_vertices(gl, draw.wallpaper, vertices);
    }

    fn draw_in_rect(
        &self,
        gl: &glow::Context,
        output_width: i32,
        output_height: i32,
        draw: CanvasTileDraw,
    ) {
        let vertices = wallpaper_vertices_in_rect(output_width, output_height, draw);
        self.draw_vertices(gl, draw.wallpaper, vertices);
    }

    fn draw_vertices(&self, gl: &glow::Context, wallpaper: WallpaperTexture, vertices: [f32; 16]) {
        let mut vertex_bytes = Vec::with_capacity(vertices.len() * size_of::<f32>());
        for vertex in vertices {
            vertex_bytes.extend_from_slice(&vertex.to_ne_bytes());
        }

        unsafe {
            gl.use_program(Some(self.program));
            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(glow::TEXTURE_2D, Some(wallpaper.texture));
            gl.uniform_1_i32(self.texture_uniform.as_ref(), 0);

            gl.bind_buffer(glow::ARRAY_BUFFER, Some(self.vertex_buffer));
            gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, &vertex_bytes, glow::DYNAMIC_DRAW);

            gl.enable_vertex_attrib_array(self.position_attribute);
            gl.vertex_attrib_pointer_f32(
                self.position_attribute,
                2,
                glow::FLOAT,
                false,
                4 * i32::try_from(size_of::<f32>()).expect("f32 size fits i32"),
                0,
            );
            gl.enable_vertex_attrib_array(self.texcoord_attribute);
            gl.vertex_attrib_pointer_f32(
                self.texcoord_attribute,
                2,
                glow::FLOAT,
                false,
                4 * i32::try_from(size_of::<f32>()).expect("f32 size fits i32"),
                2 * i32::try_from(size_of::<f32>()).expect("f32 size fits i32"),
            );

            gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);

            gl.disable_vertex_attrib_array(self.position_attribute);
            gl.disable_vertex_attrib_array(self.texcoord_attribute);
            gl.bind_buffer(glow::ARRAY_BUFFER, None);
            gl.bind_texture(glow::TEXTURE_2D, None);
            gl.use_program(None);
        }
    }
}

struct FadeRenderer {
    program: glow::NativeProgram,
    vertex_buffer: glow::NativeBuffer,
    position_attribute: u32,
    old_texture_uniform: Option<glow::NativeUniformLocation>,
    new_texture_uniform: Option<glow::NativeUniformLocation>,
    old_present_uniform: Option<glow::NativeUniformLocation>,
    old_rect_uniform: Option<glow::NativeUniformLocation>,
    new_rect_uniform: Option<glow::NativeUniformLocation>,
    clear_color_uniform: Option<glow::NativeUniformLocation>,
    mix_uniform: Option<glow::NativeUniformLocation>,
}

impl FadeRenderer {
    fn new(gl: &glow::Context) -> Result<Self, String> {
        let program = unsafe {
            let vertex_shader = compile_shader(gl, glow::VERTEX_SHADER, FADE_VERTEX_SHADER_SOURCE)?;
            let fragment_shader =
                compile_shader(gl, glow::FRAGMENT_SHADER, FADE_FRAGMENT_SHADER_SOURCE)?;
            let program = gl
                .create_program()
                .map_err(|error| format!("failed to create fade GL program: {error}"))?;
            gl.attach_shader(program, vertex_shader);
            gl.attach_shader(program, fragment_shader);
            gl.link_program(program);
            gl.detach_shader(program, vertex_shader);
            gl.detach_shader(program, fragment_shader);
            gl.delete_shader(vertex_shader);
            gl.delete_shader(fragment_shader);
            if !gl.get_program_link_status(program) {
                let log = gl.get_program_info_log(program);
                gl.delete_program(program);
                return Err(format!("failed to link fade shader program: {log}"));
            }
            program
        };
        let vertex_buffer = unsafe {
            gl.create_buffer()
                .map_err(|error| format!("failed to create fade GL vertex buffer: {error}"))?
        };
        let position_attribute = unsafe {
            gl.get_attrib_location(program, "a_position")
                .ok_or_else(|| "fade shader attribute a_position is missing".to_owned())?
        };

        Ok(Self {
            program,
            vertex_buffer,
            position_attribute,
            old_texture_uniform: unsafe { gl.get_uniform_location(program, "u_old_texture") },
            new_texture_uniform: unsafe { gl.get_uniform_location(program, "u_new_texture") },
            old_present_uniform: unsafe { gl.get_uniform_location(program, "u_old_present") },
            old_rect_uniform: unsafe { gl.get_uniform_location(program, "u_old_rect") },
            new_rect_uniform: unsafe { gl.get_uniform_location(program, "u_new_rect") },
            clear_color_uniform: unsafe { gl.get_uniform_location(program, "u_clear_color") },
            mix_uniform: unsafe { gl.get_uniform_location(program, "u_mix") },
        })
    }

    fn draw(
        &self,
        gl: &glow::Context,
        output_width: i32,
        output_height: i32,
        frame: FadeFrame,
        mix: f32,
    ) {
        let vertices = [-1.0_f32, -1.0, 1.0, -1.0, -1.0, 1.0, 1.0, 1.0];
        let mut vertex_bytes = Vec::with_capacity(vertices.len() * size_of::<f32>());
        for vertex in vertices {
            vertex_bytes.extend_from_slice(&vertex.to_ne_bytes());
        }
        let old_rect = frame.old.map_or([0.0; 4], |old| {
            normalized_wallpaper_rect(output_width, output_height, old, frame.old_scale_mode)
        });
        let new_rect =
            normalized_wallpaper_rect(output_width, output_height, frame.new, frame.new_scale_mode);

        unsafe {
            gl.disable(glow::BLEND);
            gl.disable(glow::SCISSOR_TEST);
            gl.use_program(Some(self.program));
            gl.uniform_1_f32(
                self.old_present_uniform.as_ref(),
                if frame.old.is_some() { 1.0 } else { 0.0 },
            );
            gl.uniform_4_f32(
                self.old_rect_uniform.as_ref(),
                old_rect[0],
                old_rect[1],
                old_rect[2],
                old_rect[3],
            );
            gl.uniform_4_f32(
                self.new_rect_uniform.as_ref(),
                new_rect[0],
                new_rect[1],
                new_rect[2],
                new_rect[3],
            );
            gl.uniform_4_f32(
                self.clear_color_uniform.as_ref(),
                frame.clear_color.r,
                frame.clear_color.g,
                frame.clear_color.b,
                frame.clear_color.a,
            );
            gl.uniform_1_f32(self.mix_uniform.as_ref(), mix);

            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(
                glow::TEXTURE_2D,
                Some(frame.old.unwrap_or(frame.new).texture),
            );
            gl.uniform_1_i32(self.old_texture_uniform.as_ref(), 0);
            gl.active_texture(glow::TEXTURE1);
            gl.bind_texture(glow::TEXTURE_2D, Some(frame.new.texture));
            gl.uniform_1_i32(self.new_texture_uniform.as_ref(), 1);

            gl.bind_buffer(glow::ARRAY_BUFFER, Some(self.vertex_buffer));
            gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, &vertex_bytes, glow::DYNAMIC_DRAW);
            gl.enable_vertex_attrib_array(self.position_attribute);
            gl.vertex_attrib_pointer_f32(
                self.position_attribute,
                2,
                glow::FLOAT,
                false,
                2 * i32::try_from(size_of::<f32>()).expect("f32 size fits i32"),
                0,
            );
            gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);

            gl.disable_vertex_attrib_array(self.position_attribute);
            gl.bind_buffer(glow::ARRAY_BUFFER, None);
            gl.bind_texture(glow::TEXTURE_2D, None);
            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(glow::TEXTURE_2D, None);
            gl.use_program(None);
        }
    }
}

const VERTEX_SHADER_SOURCE: &str = r"
attribute vec2 a_position;
attribute vec2 a_texcoord;
varying vec2 v_texcoord;

void main() {
    v_texcoord = a_texcoord;
    gl_Position = vec4(a_position, 0.0, 1.0);
}
";

const FRAGMENT_SHADER_SOURCE: &str = r"
precision mediump float;
varying vec2 v_texcoord;
uniform sampler2D u_texture;

void main() {
    gl_FragColor = texture2D(u_texture, v_texcoord);
}
";

const FADE_VERTEX_SHADER_SOURCE: &str = r"
attribute vec2 a_position;
varying vec2 v_output_uv;

void main() {
    v_output_uv = vec2(a_position.x * 0.5 + 0.5, 0.5 - a_position.y * 0.5);
    gl_Position = vec4(a_position, 0.0, 1.0);
}
";

const FADE_FRAGMENT_SHADER_SOURCE: &str = r"
precision mediump float;
varying vec2 v_output_uv;
uniform sampler2D u_old_texture;
uniform sampler2D u_new_texture;
uniform float u_old_present;
uniform vec4 u_old_rect;
uniform vec4 u_new_rect;
uniform vec4 u_clear_color;
uniform float u_mix;

bool inside_rect(vec2 point, vec4 rect) {
    return point.x >= rect.x && point.y >= rect.y
        && point.x <= rect.x + rect.z && point.y <= rect.y + rect.w;
}

vec4 old_scene(vec2 point) {
    if (u_old_present < 0.5 || !inside_rect(point, u_old_rect)) {
        return u_clear_color;
    }
    vec2 uv = (point - u_old_rect.xy) / u_old_rect.zw;
    return texture2D(u_old_texture, uv);
}

vec4 new_scene(vec2 point) {
    if (!inside_rect(point, u_new_rect)) {
        return u_clear_color;
    }
    vec2 uv = (point - u_new_rect.xy) / u_new_rect.zw;
    return texture2D(u_new_texture, uv);
}

void main() {
    gl_FragColor = mix(old_scene(v_output_uv), new_scene(v_output_uv), u_mix);
}
";

fn clear_gl(gl: &glow::Context, width: i32, height: i32, color: Color) {
    unsafe {
        gl.viewport(0, 0, width, height);
        gl.clear_color(color.r, color.g, color.b, color.a);
        gl.clear(glow::COLOR_BUFFER_BIT);
    }
}

fn render_screen_push(
    renderer: &GlRenderer,
    gl: &glow::Context,
    width: i32,
    height: i32,
    frame: &PushFrame,
    old_offset: Offset,
    new_offset: Offset,
) {
    if let Some(old) = frame.old {
        renderer.draw_with_offset(gl, width, height, old, frame.old_scale_mode, old_offset);
    }
    renderer.draw_with_offset(
        gl,
        width,
        height,
        frame.new,
        frame.new_scale_mode,
        new_offset,
    );
}

fn render_portal_push_frame(
    renderer: &GlRenderer,
    gl: &glow::Context,
    width: i32,
    height: i32,
    frame: &PushFrame,
    fallback_offset: Offset,
    eased_progress: f32,
) {
    let Some(old) = frame.old else {
        renderer.draw_with_offset(
            gl,
            width,
            height,
            frame.new,
            frame.new_scale_mode,
            fallback_offset,
        );
        return;
    };

    let (old_offset, new_offset) = portal_offsets(PortalOffsets {
        output_width: width,
        output_height: height,
        old,
        old_scale_mode: frame.old_scale_mode,
        new: frame.new,
        new_scale_mode: frame.new_scale_mode,
        direction: frame.direction,
        progress: eased_progress,
    });
    render_portal_image(
        renderer,
        gl,
        width,
        height,
        PortalImageDraw {
            frame,
            wallpaper: old,
            image: PortalImage::Old,
            offset: old_offset,
            pan: false,
            progress: 0.0,
        },
    );
    render_portal_image(
        renderer,
        gl,
        width,
        height,
        PortalImageDraw {
            frame,
            wallpaper: frame.new,
            image: PortalImage::New,
            offset: new_offset,
            pan: false,
            progress: 0.0,
        },
    );
}

fn render_pan_push_frame(
    renderer: &GlRenderer,
    gl: &glow::Context,
    width: i32,
    height: i32,
    frame: &PushFrame,
    eased_progress: f32,
) {
    unsafe {
        gl.enable(glow::SCISSOR_TEST);
    }
    if let Some(old) = frame.old {
        set_scissor(
            gl,
            portal_scissor(
                width,
                height,
                frame.direction,
                eased_progress,
                PortalImage::Old,
            ),
        );
        render_portal_image(
            renderer,
            gl,
            width,
            height,
            PortalImageDraw {
                frame,
                wallpaper: old,
                image: PortalImage::Old,
                offset: Offset { x: 0.0, y: 0.0 },
                pan: true,
                progress: eased_progress,
            },
        );
    }
    set_scissor(
        gl,
        portal_scissor(
            width,
            height,
            frame.direction,
            eased_progress,
            PortalImage::New,
        ),
    );
    render_portal_image(
        renderer,
        gl,
        width,
        height,
        PortalImageDraw {
            frame,
            wallpaper: frame.new,
            image: PortalImage::New,
            offset: Offset { x: 0.0, y: 0.0 },
            pan: true,
            progress: eased_progress,
        },
    );
    unsafe {
        gl.disable(glow::SCISSOR_TEST);
    }
}

fn render_portal_image(
    renderer: &GlRenderer,
    gl: &glow::Context,
    width: i32,
    height: i32,
    draw: PortalImageDraw<'_>,
) {
    renderer.draw_portal_push(
        gl,
        PortalDraw {
            output_width: width,
            output_height: height,
            wallpaper: draw.wallpaper,
            scale_mode: match draw.image {
                PortalImage::Old => draw.frame.old_scale_mode,
                PortalImage::New => draw.frame.new_scale_mode,
            },
            direction: draw.frame.direction,
            progress: draw.progress,
            image: draw.image,
            offset: draw.offset,
            pan: draw.pan,
        },
    );
}

fn compile_shader(
    gl: &glow::Context,
    shader_type: u32,
    source: &str,
) -> Result<glow::NativeShader, String> {
    unsafe {
        let shader = gl
            .create_shader(shader_type)
            .map_err(|error| format!("failed to create GL shader: {error}"))?;
        gl.shader_source(shader, source);
        gl.compile_shader(shader);
        if gl.get_shader_compile_status(shader) {
            Ok(shader)
        } else {
            let log = gl.get_shader_info_log(shader);
            gl.delete_shader(shader);
            Err(format!("failed to compile GL shader: {log}"))
        }
    }
}

#[allow(clippy::cast_precision_loss)]
fn wallpaper_vertices(
    output_width: i32,
    output_height: i32,
    wallpaper: WallpaperTexture,
    scale_mode: ScaleMode,
    offset: Offset,
) -> [f32; 16] {
    let output_width = output_width as f32;
    let output_height = output_height as f32;
    let rect = scaled_wallpaper_rect(output_width, output_height, wallpaper, scale_mode);
    // Build vertices from the portion of the scaled image that is visible on
    // the output, not from the full scaled image quad. This keeps push:left and
    // push:right correct on portrait outputs where fill-mode landscape images
    // are much wider than the output.
    let visible_left = rect.x.max(0.0);
    let visible_right = (rect.x + rect.width).min(output_width);
    let visible_top = rect.y.max(0.0);
    let visible_bottom = (rect.y + rect.height).min(output_height);

    if visible_left >= visible_right || visible_top >= visible_bottom {
        return [0.0; 16];
    }

    let texture_left = (visible_left - rect.x) / rect.width;
    let texture_right = (visible_right - rect.x) / rect.width;
    let texture_top = (visible_top - rect.y) / rect.height;
    let texture_bottom = (visible_bottom - rect.y) / rect.height;

    let translate_x = offset.x * output_width;
    let translate_y = offset.y * output_height;
    let left = ((visible_left + translate_x) / output_width) * 2.0 - 1.0;
    let right = ((visible_right + translate_x) / output_width) * 2.0 - 1.0;
    let top = 1.0 - ((visible_top + translate_y) / output_height) * 2.0;
    let bottom = 1.0 - ((visible_bottom + translate_y) / output_height) * 2.0;

    [
        left,
        bottom,
        texture_left,
        texture_bottom,
        right,
        bottom,
        texture_right,
        texture_bottom,
        left,
        top,
        texture_left,
        texture_top,
        right,
        top,
        texture_right,
        texture_top,
    ]
}

#[allow(clippy::cast_precision_loss)]
fn wallpaper_vertices_in_rect(
    output_width: i32,
    output_height: i32,
    draw: CanvasTileDraw,
) -> [f32; 16] {
    if draw.rect.width <= 0.0 || draw.rect.height <= 0.0 {
        return [0.0; 16];
    }

    let output_width = output_width as f32;
    let output_height = output_height as f32;
    let local = scaled_wallpaper_rect(
        draw.rect.width,
        draw.rect.height,
        draw.wallpaper,
        draw.scale_mode,
    );
    let rect = WallpaperRect {
        x: draw.rect.x + local.x,
        y: draw.rect.y + local.y,
        width: local.width,
        height: local.height,
    };

    let visible_left = rect.x.max(0.0);
    let visible_right = (rect.x + rect.width).min(output_width);
    let visible_top = rect.y.max(0.0);
    let visible_bottom = (rect.y + rect.height).min(output_height);

    if visible_left >= visible_right || visible_top >= visible_bottom {
        return [0.0; 16];
    }

    let texture_left = (visible_left - rect.x) / rect.width;
    let texture_right = (visible_right - rect.x) / rect.width;
    let texture_top = (visible_top - rect.y) / rect.height;
    let texture_bottom = (visible_bottom - rect.y) / rect.height;

    let left = (visible_left / output_width) * 2.0 - 1.0;
    let right = (visible_right / output_width) * 2.0 - 1.0;
    let top = 1.0 - (visible_top / output_height) * 2.0;
    let bottom = 1.0 - (visible_bottom / output_height) * 2.0;

    [
        left,
        bottom,
        texture_left,
        texture_bottom,
        right,
        bottom,
        texture_right,
        texture_bottom,
        left,
        top,
        texture_left,
        texture_top,
        right,
        top,
        texture_right,
        texture_top,
    ]
}

#[allow(clippy::cast_precision_loss)]
fn portal_wallpaper_vertices(draw: PortalDraw) -> [f32; 16] {
    let output_width = draw.output_width as f32;
    let output_height = draw.output_height as f32;
    let rect = scaled_wallpaper_rect(output_width, output_height, draw.wallpaper, draw.scale_mode);
    let (pan_x, pan_y) = if draw.pan {
        portal_translation(
            output_width,
            output_height,
            rect,
            draw.direction,
            draw.progress,
            draw.image,
        )
    } else {
        (0.0, 0.0)
    };
    let translate_x = pan_x + draw.offset.x * output_width;
    let translate_y = pan_y + draw.offset.y * output_height;

    let left = ((rect.x + translate_x) / output_width) * 2.0 - 1.0;
    let right = ((rect.x + rect.width + translate_x) / output_width) * 2.0 - 1.0;
    let top = 1.0 - ((rect.y + translate_y) / output_height) * 2.0;
    let bottom = 1.0 - ((rect.y + rect.height + translate_y) / output_height) * 2.0;

    [
        left, bottom, 0.0, 1.0, right, bottom, 1.0, 1.0, left, top, 0.0, 0.0, right, top, 1.0, 0.0,
    ]
}

#[derive(Clone, Copy)]
struct WallpaperRect {
    x: f32,
    y: f32,
    width: f32,
    height: f32,
}

#[allow(clippy::cast_precision_loss)]
fn scaled_wallpaper_rect(
    output_width: f32,
    output_height: f32,
    wallpaper: WallpaperTexture,
    scale_mode: ScaleMode,
) -> WallpaperRect {
    let image_width = wallpaper.width as f32;
    let image_height = wallpaper.height as f32;

    let (width, height) = match scale_mode {
        ScaleMode::Fill => {
            let scale = (output_width / image_width).max(output_height / image_height);
            (image_width * scale, image_height * scale)
        }
        ScaleMode::Fit => {
            let scale = (output_width / image_width).min(output_height / image_height);
            (image_width * scale, image_height * scale)
        }
        ScaleMode::Center => {
            let scale = 1.0_f32.min((output_width / image_width).min(output_height / image_height));
            (image_width * scale, image_height * scale)
        }
        ScaleMode::Stretch => (output_width, output_height),
    };

    WallpaperRect {
        x: (output_width - width) / 2.0,
        y: (output_height - height) / 2.0,
        width,
        height,
    }
}

#[allow(clippy::cast_precision_loss)]
fn normalized_wallpaper_rect(
    output_width: i32,
    output_height: i32,
    wallpaper: WallpaperTexture,
    scale_mode: ScaleMode,
) -> [f32; 4] {
    let output_width = output_width as f32;
    let output_height = output_height as f32;
    let rect = scaled_wallpaper_rect(output_width, output_height, wallpaper, scale_mode);
    [
        rect.x / output_width,
        rect.y / output_height,
        rect.width / output_width,
        rect.height / output_height,
    ]
}

fn portal_translation(
    output_width: f32,
    output_height: f32,
    rect: WallpaperRect,
    direction: mural_ipc::PushDirection,
    progress: f32,
    image: PortalImage,
) -> (f32, f32) {
    let progress = progress.clamp(0.0, 1.0);
    let remaining = 1.0 - progress;

    match (direction, image) {
        (mural_ipc::PushDirection::Left, PortalImage::Old) => {
            (-(rect.x + rect.width) * progress, 0.0)
        }
        (mural_ipc::PushDirection::Left, PortalImage::New) => {
            ((output_width - rect.x) * remaining, 0.0)
        }
        (mural_ipc::PushDirection::Right, PortalImage::Old) => {
            ((output_width - rect.x) * progress, 0.0)
        }
        (mural_ipc::PushDirection::Right, PortalImage::New) => {
            (-(rect.x + rect.width) * remaining, 0.0)
        }
        (mural_ipc::PushDirection::Up, PortalImage::Old) => {
            (0.0, -(rect.y + rect.height) * progress)
        }
        (mural_ipc::PushDirection::Up, PortalImage::New) => {
            (0.0, (output_height - rect.y) * remaining)
        }
        (mural_ipc::PushDirection::Down, PortalImage::Old) => {
            (0.0, (output_height - rect.y) * progress)
        }
        (mural_ipc::PushDirection::Down, PortalImage::New) => {
            (0.0, -(rect.y + rect.height) * remaining)
        }
    }
}

#[allow(clippy::cast_precision_loss)]
fn portal_offsets(draw: PortalOffsets) -> (Offset, Offset) {
    let output_width = draw.output_width as f32;
    let output_height = draw.output_height as f32;
    let old_rect =
        scaled_wallpaper_rect(output_width, output_height, draw.old, draw.old_scale_mode);
    let new_rect =
        scaled_wallpaper_rect(output_width, output_height, draw.new, draw.new_scale_mode);
    let progress = draw.progress.clamp(0.0, 1.0);

    match draw.direction {
        mural_ipc::PushDirection::Left => {
            let travel = old_rect.x + old_rect.width - new_rect.x;
            (
                Offset {
                    x: -(travel * progress) / output_width,
                    y: 0.0,
                },
                Offset {
                    x: (travel * (1.0 - progress)) / output_width,
                    y: 0.0,
                },
            )
        }
        mural_ipc::PushDirection::Right => {
            let travel = old_rect.x - (new_rect.x + new_rect.width);
            (
                Offset {
                    x: -(travel * progress) / output_width,
                    y: 0.0,
                },
                Offset {
                    x: (travel * (1.0 - progress)) / output_width,
                    y: 0.0,
                },
            )
        }
        mural_ipc::PushDirection::Up => {
            let travel = old_rect.y + old_rect.height - new_rect.y;
            (
                Offset {
                    x: 0.0,
                    y: -(travel * progress) / output_height,
                },
                Offset {
                    x: 0.0,
                    y: (travel * (1.0 - progress)) / output_height,
                },
            )
        }
        mural_ipc::PushDirection::Down => {
            let travel = old_rect.y - (new_rect.y + new_rect.height);
            (
                Offset {
                    x: 0.0,
                    y: -(travel * progress) / output_height,
                },
                Offset {
                    x: 0.0,
                    y: (travel * (1.0 - progress)) / output_height,
                },
            )
        }
    }
}

fn portal_scissor(
    output_width: i32,
    output_height: i32,
    direction: mural_ipc::PushDirection,
    progress: f32,
    image: PortalImage,
) -> ScissorRect {
    let progress = progress.clamp(0.0, 1.0);

    match (direction, image) {
        (mural_ipc::PushDirection::Left, PortalImage::Old) => ScissorRect {
            x: 0,
            y: 0,
            width: split_pixels(output_width, 1.0 - progress),
            height: output_height,
        },
        (mural_ipc::PushDirection::Left, PortalImage::New) => {
            let split = split_pixels(output_width, 1.0 - progress);
            ScissorRect {
                x: split,
                y: 0,
                width: output_width - split,
                height: output_height,
            }
        }
        (mural_ipc::PushDirection::Right, PortalImage::Old) => {
            let split = split_pixels(output_width, progress);
            ScissorRect {
                x: split,
                y: 0,
                width: output_width - split,
                height: output_height,
            }
        }
        (mural_ipc::PushDirection::Right, PortalImage::New) => ScissorRect {
            x: 0,
            y: 0,
            width: split_pixels(output_width, progress),
            height: output_height,
        },
        (mural_ipc::PushDirection::Up, PortalImage::Old) => {
            let split = split_pixels(output_height, 1.0 - progress);
            ScissorRect {
                x: 0,
                y: output_height - split,
                width: output_width,
                height: split,
            }
        }
        (mural_ipc::PushDirection::Up, PortalImage::New) => {
            let split = split_pixels(output_height, 1.0 - progress);
            ScissorRect {
                x: 0,
                y: 0,
                width: output_width,
                height: output_height - split,
            }
        }
        (mural_ipc::PushDirection::Down, PortalImage::Old) => {
            let split = split_pixels(output_height, progress);
            ScissorRect {
                x: 0,
                y: 0,
                width: output_width,
                height: output_height - split,
            }
        }
        (mural_ipc::PushDirection::Down, PortalImage::New) => {
            let split = split_pixels(output_height, progress);
            ScissorRect {
                x: 0,
                y: output_height - split,
                width: output_width,
                height: split,
            }
        }
    }
}

#[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
fn canvas_tile_scissor(output_width: i32, output_height: i32, rect: WallpaperRect) -> ScissorRect {
    let left = rect.x.floor().max(0.0);
    let right = (rect.x + rect.width).ceil().min(output_width as f32);
    let top = rect.y.floor().max(0.0);
    let bottom = (rect.y + rect.height).ceil().min(output_height as f32);

    let x = left as i32;
    let width = (right - left).max(0.0) as i32;
    let y_from_top = top as i32;
    let height = (bottom - top).max(0.0) as i32;
    ScissorRect {
        x,
        y: output_height
            .saturating_sub(y_from_top)
            .saturating_sub(height),
        width,
        height,
    }
}

#[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
fn split_pixels(size: i32, fraction: f32) -> i32 {
    (size as f32 * fraction.clamp(0.0, 1.0)).round() as i32
}

fn set_scissor(gl: &glow::Context, rect: ScissorRect) {
    unsafe {
        gl.scissor(rect.x, rect.y, rect.width.max(0), rect.height.max(0));
    }
}

fn canvas_tile_texture(
    frame: &CanvasFrame<'_>,
    index: usize,
    tile: &CanvasTile,
) -> Option<(WallpaperTexture, ScaleMode)> {
    if index == frame.target_index {
        return frame
            .new
            .map(|wallpaper| (wallpaper, frame.new_scale_mode))
            .or_else(|| {
                tile.texture
                    .map(|wallpaper| (wallpaper, frame.new_scale_mode))
            });
    }
    if index == frame.old_index {
        return frame
            .old
            .map(|wallpaper| (wallpaper, frame.old_scale_mode))
            .or_else(|| {
                tile.texture
                    .map(|wallpaper| (wallpaper, frame.new_scale_mode))
            });
    }
    tile.texture
        .map(|wallpaper| (wallpaper, frame.new_scale_mode))
}

fn canvas_tile_wallpaper(
    frame: &CanvasFrame<'_>,
    index: usize,
    tile: &CanvasTile,
) -> Option<WallpaperTexture> {
    canvas_tile_texture(frame, index, tile).map(|(wallpaper, _)| wallpaper)
}

#[allow(clippy::cast_precision_loss)]
fn canvas_mode_aspects(frame: &CanvasFrame<'_>, output: Size) -> Vec<f32> {
    let output_aspect = output.width as f32 / output.height.max(1) as f32;
    frame
        .tiles
        .iter()
        .enumerate()
        .map(|(index, tile)| {
            canvas_tile_wallpaper(frame, index, tile).map_or(1.0, |wallpaper| {
                let image_aspect = wallpaper.width.max(1) as f32 / wallpaper.height.max(1) as f32;
                (image_aspect / output_aspect).max(f32::EPSILON)
            })
        })
        .collect()
}

fn canvas_layout_input<'a>(
    frame: &CanvasFrame<'_>,
    output: Size,
    aspects: &'a [f32],
) -> CanvasLayoutInput<'a> {
    CanvasLayoutInput {
        tile_count: frame.tiles.len(),
        old_index: frame.old_index,
        target_index: frame.target_index,
        pan_axis: frame.pan_axis,
        overview_scale: frame.overview_scale,
        output,
        aspects,
    }
}

#[allow(clippy::cast_precision_loss)]
fn canvas_draw_rect_for_view(
    rect: Rect,
    transform: CanvasModeTransform,
    span: Option<&CanvasSpan>,
    output_width: f32,
    output_height: f32,
) -> WallpaperRect {
    canvas_wallpaper_rect_for_view(
        canvas_rect_apply_transform(rect, transform),
        span,
        output_width,
        output_height,
    )
}

#[allow(clippy::cast_precision_loss)]
fn canvas_wallpaper_rect_for_view(
    rect: Rect,
    span: Option<&CanvasSpan>,
    output_width: f32,
    output_height: f32,
) -> WallpaperRect {
    let mut x = rect.x;
    let mut y = rect.y;
    let mut width = rect.width;
    let mut height = rect.height;

    if let Some(span) = span {
        let desktop_width = span.desktop_width.max(1) as f32;
        let desktop_height = span.desktop_height.max(1) as f32;
        let viewport_x = span.viewport_x as f32;
        let viewport_y = span.viewport_y as f32;
        let viewport_width = span.viewport_width.max(1) as f32;
        let viewport_height = span.viewport_height.max(1) as f32;
        x = (x * desktop_width - viewport_x) / viewport_width;
        y = (y * desktop_height - viewport_y) / viewport_height;
        width = width * desktop_width / viewport_width;
        height = height * desktop_height / viewport_height;
    }

    WallpaperRect {
        x: x * output_width,
        y: y * output_height,
        width: width * output_width,
        height: height * output_height,
    }
}

#[allow(clippy::cast_precision_loss)]
fn world_tile_rect_for_view(
    tile: &WorldTileTexture,
    tile_cells: usize,
    view: Rect,
    output_width: i32,
    output_height: i32,
) -> WallpaperRect {
    let tile_cells = world_lod_tile_cells(tile_cells, tile.lod) as f32;
    world_rect_for_view(
        Rect {
            x: tile.tile.column as f32 * tile_cells,
            y: tile.tile.row as f32 * tile_cells,
            width: tile_cells,
            height: tile_cells,
        },
        view,
        output_width,
        output_height,
    )
}

fn world_cell_draw_rect(
    layout: WorldLayout,
    index: usize,
    view: Rect,
    output_width: i32,
    output_height: i32,
) -> Option<WallpaperRect> {
    let rect = world_cell_rect(layout, index)?;
    Some(world_rect_for_view(rect, view, output_width, output_height))
}

#[allow(clippy::cast_precision_loss)]
fn world_rect_for_view(
    rect: Rect,
    view: Rect,
    output_width: i32,
    output_height: i32,
) -> WallpaperRect {
    let view_width = view.width.max(f32::EPSILON);
    let view_height = view.height.max(f32::EPSILON);
    WallpaperRect {
        x: ((rect.x - view.x) / view_width) * output_width as f32,
        y: ((rect.y - view.y) / view_height) * output_height as f32,
        width: (rect.width / view_width) * output_width as f32,
        height: (rect.height / view_height) * output_height as f32,
    }
}

#[allow(clippy::cast_precision_loss)]
fn canvas_full_thumbnail_final_transform(
    frame: &CanvasFrame<'_>,
    index: usize,
    rects: &[Rect],
    old: bool,
    output_width: i32,
    output_height: i32,
) -> CanvasModeTransform {
    let Some(rect) = rects.get(index).copied() else {
        return CanvasModeTransform::identity();
    };
    let texture = if old { frame.old } else { frame.new }
        .or_else(|| frame.tiles.get(index).and_then(|tile| tile.texture));
    let Some(texture) = texture else {
        return canvas_final_transform(index, rects);
    };
    let scale_mode = if old {
        frame.old_scale_mode
    } else {
        frame.new_scale_mode
    };
    let final_rect = scaled_wallpaper_rect(
        output_width as f32,
        output_height as f32,
        texture,
        scale_mode,
    );
    let normalized = Rect {
        x: final_rect.x / output_width as f32,
        y: final_rect.y / output_height as f32,
        width: final_rect.width / output_width as f32,
        height: final_rect.height / output_height as f32,
    };
    let scale_x = if rect.width > f32::EPSILON {
        normalized.width / rect.width
    } else if rect.height > f32::EPSILON {
        normalized.height / rect.height
    } else {
        1.0
    };
    let scale_y = if rect.height > f32::EPSILON {
        normalized.height / rect.height
    } else {
        scale_x
    };
    CanvasModeTransform {
        scale_x,
        scale_y,
        translate_x: normalized.x - rect.x * scale_x,
        translate_y: normalized.y - rect.y * scale_y,
    }
}

fn gl_i32(value: u32) -> i32 {
    i32::try_from(value).expect("GL enum value fits i32")
}

fn render_push_direction(direction: mural_ipc::PushDirection) -> RenderPushDirection {
    match direction {
        mural_ipc::PushDirection::Up => RenderPushDirection::Up,
        mural_ipc::PushDirection::Down => RenderPushDirection::Down,
        mural_ipc::PushDirection::Left => RenderPushDirection::Left,
        mural_ipc::PushDirection::Right => RenderPushDirection::Right,
    }
}

fn render_easing(easing: mural_ipc::Easing) -> RenderEasing {
    match easing {
        mural_ipc::Easing::Linear => RenderEasing::Linear,
        mural_ipc::Easing::EaseOutCubic => RenderEasing::EaseOutCubic,
        mural_ipc::Easing::EaseInOutCubic => RenderEasing::EaseInOutCubic,
    }
}

#[derive(Clone, Copy)]
pub(crate) struct Color {
    r: f32,
    g: f32,
    b: f32,
    a: f32,
}

impl Color {
    pub(crate) fn parse(input: &str) -> Result<Self, String> {
        let hex = input.strip_prefix('#').unwrap_or(input);
        if hex.len() != 6 {
            return Err(format!("clear color must be #rrggbb, got {input}"));
        }

        let value = u32::from_str_radix(hex, 16)
            .map_err(|error| format!("invalid clear color {input}: {error}"))?;
        let r = u8::try_from((value >> 16) & 0xff).unwrap();
        let g = u8::try_from((value >> 8) & 0xff).unwrap();
        let b = u8::try_from(value & 0xff).unwrap();

        Ok(Self {
            r: f32::from(r) / 255.0,
            g: f32::from(g) / 255.0,
            b: f32::from(b) / 255.0,
            a: 1.0,
        })
    }
}

impl Default for Color {
    fn default() -> Self {
        // Slightly blue-black so a live surface is distinguishable from a blank screen.
        Self {
            r: 0.015,
            g: 0.018,
            b: 0.025,
            a: 1.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_close(left: f32, right: f32) {
        assert!(
            (left - right).abs() < 0.0001,
            "expected {left} to be close to {right}"
        );
    }

    #[allow(clippy::cast_precision_loss)]
    fn image_aspects_for_output(images: &[(u32, u32)], output: Size) -> Vec<f32> {
        let output_aspect = output.width as f32 / output.height.max(1) as f32;
        images
            .iter()
            .map(|(width, height)| {
                let image_aspect = *width as f32 / (*height).max(1) as f32;
                image_aspect / output_aspect
            })
            .collect()
    }

    #[allow(clippy::cast_precision_loss)]
    fn assert_rect_preserves_image_aspect(rect: Rect, image: (u32, u32), output: Size) {
        let displayed_aspect =
            (rect.width * output.width as f32) / (rect.height * output.height.max(1) as f32);
        let image_aspect = image.0 as f32 / image.1.max(1) as f32;
        assert_close(displayed_aspect, image_aspect);
    }

    #[allow(clippy::cast_precision_loss)]
    fn assert_rect_covers_grid_slot(rect: Rect, grid: Grid, index: usize) {
        let columns = grid.columns.max(1);
        let column = (index % columns) as f32;
        let row = (index / columns) as f32;
        assert!(
            rect.x <= column + 0.0001,
            "rect {index} left edge does not cover its slot"
        );
        assert!(
            rect.y <= row + 0.0001,
            "rect {index} top edge does not cover its slot"
        );
        assert!(
            rect.x + rect.width >= column + 1.0 - 0.0001,
            "rect {index} right edge does not cover its slot"
        );
        assert!(
            rect.y + rect.height >= row + 1.0 - 0.0001,
            "rect {index} bottom edge does not cover its slot"
        );
        assert_close(rect.x + rect.width / 2.0, column + 0.5);
        assert_close(rect.y + rect.height / 2.0, row + 0.5);
    }

    fn rect_center(rect: Rect) -> (f32, f32) {
        (rect.x + rect.width / 2.0, rect.y + rect.height / 2.0)
    }

    fn unit_row_rects(count: usize) -> Vec<Rect> {
        (0..count)
            .scan(0.0, |x, _| {
                let rect = Rect {
                    x: *x,
                    y: 0.0,
                    width: 1.0,
                    height: 1.0,
                };
                *x += 1.0;
                Some(rect)
            })
            .collect()
    }

    fn unit_grid_rects(columns: usize, count: usize) -> Vec<Rect> {
        let mut x = 0.0;
        let mut y = 0.0;
        let mut rects = Vec::with_capacity(count);
        for index in 0..count {
            rects.push(Rect {
                x,
                y,
                width: 1.0,
                height: 1.0,
            });
            if (index + 1).is_multiple_of(columns.max(1)) {
                x = 0.0;
                y += 1.0;
            } else {
                x += 1.0;
            }
        }
        rects
    }

    fn test_tiles(count: usize) -> Vec<CanvasTile> {
        (0..count)
            .map(|index| CanvasTile {
                path: format!("wall-{index}"),
                texture: None,
            })
            .collect()
    }

    fn test_canvas_frame(
        tiles: &[CanvasTile],
        mode: CanvasMode,
        pan_axis: CanvasPanAxis,
        old_index: usize,
        target_index: usize,
    ) -> CanvasFrame<'_> {
        CanvasFrame {
            old: None,
            old_scale_mode: ScaleMode::Fill,
            new: None,
            new_scale_mode: ScaleMode::Fill,
            clear_color: Color::default(),
            easing: mural_ipc::Easing::Linear,
            tiles,
            old_index,
            target_index,
            zoom_out_ms: 1,
            pan_ms: 1,
            zoom_in_ms: 1,
            mode,
            pan_axis,
            overview_scale: 0.25,
            span: None,
            progress: 0.5,
        }
    }

    #[allow(clippy::cast_precision_loss)]
    fn canvas_grid_slot_center(grid: Grid, index: usize) -> CanvasPoint {
        let columns = grid.columns.max(1);
        CanvasPoint {
            x: (index % columns) as f32 + 0.5,
            y: (index / columns) as f32 + 0.5,
        }
    }

    fn test_mode_focus_centers(
        mode: CanvasMode,
        pan_axis: CanvasPanAxis,
        output: Size,
        old_index: usize,
        target_index: usize,
    ) -> (CanvasPoint, CanvasPoint) {
        let tiles = test_tiles(55);
        let frame = test_canvas_frame(&tiles, mode, pan_axis, old_index, target_index);
        match mode {
            CanvasMode::Clipped => {
                let walk_axis = canvas_walk_axis(pan_axis, output);
                let grid = canvas_grid_for_overview_axis(
                    tiles.len(),
                    output,
                    frame.overview_scale,
                    walk_axis.is_horizontal(),
                );
                (
                    canvas_grid_slot_center(grid, old_index),
                    canvas_grid_slot_center(grid, target_index),
                )
            }
            CanvasMode::Morph => {
                let aspects = canvas_mode_aspects(&frame, output);
                let layout =
                    canvas_morph_layout(canvas_layout_input(&frame, output, &aspects)).unwrap();
                (
                    canvas_rect_center(layout.rects[layout.old_index]),
                    canvas_rect_center(layout.rects[layout.target_index]),
                )
            }
            CanvasMode::Overlap => {
                let aspects = canvas_mode_aspects(&frame, output);
                let layout =
                    canvas_overlap_layout(canvas_layout_input(&frame, output, &aspects)).unwrap();
                (
                    canvas_rect_center(layout.rects[layout.old_index]),
                    canvas_rect_center(layout.rects[layout.target_index]),
                )
            }
            CanvasMode::Collage => {
                let aspects = canvas_mode_aspects(&frame, output);
                let layout =
                    canvas_collage_layout(canvas_layout_input(&frame, output, &aspects)).unwrap();
                (
                    canvas_rect_center(layout.rects[layout.old_index]),
                    canvas_rect_center(layout.rects[layout.target_index]),
                )
            }
            CanvasMode::Span => unreachable!("span uses a separate virtual-desktop layout"),
        }
    }

    fn three_output_span() -> CanvasSpan {
        CanvasSpan {
            desktop_width: 3000,
            desktop_height: 1000,
            viewport_x: 1000,
            viewport_y: 0,
            viewport_width: 1000,
            viewport_height: 1000,
            output_index: 1,
            output_count: 3,
            slots: vec![
                CanvasSpanSlot {
                    x: 0,
                    y: 0,
                    width: 1000,
                    height: 1000,
                },
                CanvasSpanSlot {
                    x: 1000,
                    y: 0,
                    width: 1000,
                    height: 1000,
                },
                CanvasSpanSlot {
                    x: 2000,
                    y: 0,
                    width: 1000,
                    height: 1000,
                },
            ],
        }
    }

    fn uneven_three_output_span() -> CanvasSpan {
        CanvasSpan {
            desktop_width: 3000,
            desktop_height: 1600,
            viewport_x: 900,
            viewport_y: 300,
            viewport_width: 1200,
            viewport_height: 1000,
            output_index: 1,
            output_count: 3,
            slots: vec![
                CanvasSpanSlot {
                    x: 0,
                    y: 0,
                    width: 900,
                    height: 1600,
                },
                CanvasSpanSlot {
                    x: 900,
                    y: 300,
                    width: 1200,
                    height: 1000,
                },
                CanvasSpanSlot {
                    x: 2100,
                    y: 0,
                    width: 900,
                    height: 1600,
                },
            ],
        }
    }

    #[test]
    fn horizontal_morph_rects_fill_rows_without_gaps() {
        let rects = canvas_horizontal_morph_rects(
            Grid {
                columns: 3,
                rows: 2,
            },
            &[1.0, 2.0, 1.0, 3.0, 1.0, 2.0],
        );

        assert_eq!(rects.len(), 6);
        for row in 0..2 {
            let start = row * 3;
            assert_close(rects[start].x, 0.0);
            assert_close(rects[start + 1].x, rects[start].x + rects[start].width);
            assert_close(
                rects[start + 2].x,
                rects[start + 1].x + rects[start + 1].width,
            );
            assert_close(rects[start + 2].x + rects[start + 2].width, 3.0);
        }
    }

    #[test]
    fn non_span_mode_layouts_honor_horizontal_pan_axis_on_portrait_outputs() {
        let output = Size {
            width: 1080,
            height: 1920,
        };

        for mode in [
            CanvasMode::Clipped,
            CanvasMode::Morph,
            CanvasMode::Overlap,
            CanvasMode::Collage,
        ] {
            let (old, target) =
                test_mode_focus_centers(mode, CanvasPanAxis::Horizontal, output, 16, 17);

            assert!(
                target.x > old.x,
                "{mode:?} should move horizontally: old={old:?} target={target:?}"
            );
            assert_close(target.y, old.y);
        }
    }

    #[test]
    fn non_span_mode_layouts_honor_vertical_pan_axis_on_landscape_outputs() {
        let output = Size {
            width: 1920,
            height: 1080,
        };

        for mode in [
            CanvasMode::Clipped,
            CanvasMode::Morph,
            CanvasMode::Overlap,
            CanvasMode::Collage,
        ] {
            let (old, target) =
                test_mode_focus_centers(mode, CanvasPanAxis::Vertical, output, 17, 22);

            assert_close(target.x, old.x);
            assert!(
                target.y > old.y,
                "{mode:?} should move vertically: old={old:?} target={target:?}"
            );
        }
    }

    #[test]
    fn vertical_morph_rects_fill_columns_without_gaps() {
        let rects = canvas_vertical_morph_rects(
            Grid {
                columns: 2,
                rows: 3,
            },
            &[1.0, 2.0, 1.0, 3.0, 1.0, 2.0],
        );

        assert_eq!(rects.len(), 6);
        for column in 0..2 {
            let first = column;
            let second = column + 2;
            let third = column + 4;
            assert_close(rects[first].y, 0.0);
            assert_close(rects[second].y, rects[first].y + rects[first].height);
            assert_close(rects[third].y, rects[second].y + rects[second].height);
            assert_close(rects[third].y + rects[third].height, 3.0);
        }
        assert_close(rects[0].x, 0.0);
        assert_close(rects[1].x, rects[0].x + rects[0].width);
        assert_close(rects[2].x, rects[0].x);
        assert_close(rects[3].x, rects[1].x);
    }

    #[test]
    fn portrait_morph_can_pan_horizontally_while_packing_vertically() {
        let output = Size {
            width: 1080,
            height: 1920,
        };
        let grid = canvas_grid_for_overview_axis(55, output, 0.25, true);
        let rects = canvas_morph_rects(
            grid,
            &[3.0; 55],
            canvas_morph_pack_axis(output).is_horizontal(),
        );
        let old_index = 16;
        let target_index = 17;
        let old_center = canvas_rect_center(rects[old_index]);
        let target_center = canvas_rect_center(rects[target_index]);

        assert_eq!(grid.columns, 11);
        assert_eq!(grid.rows, 5);
        assert!(target_center.x > old_center.x);
        assert_close(target_center.y, old_center.y);
        for column in 0..grid.columns {
            let top = column;
            let bottom = column + grid.columns * (grid.rows - 1);
            assert_close(rects[top].y, 0.0);
            assert_close(rects[bottom].y + rects[bottom].height, 5.0);
        }
    }

    #[test]
    fn overlap_rects_leave_incomplete_edge_rough() {
        let grid = Grid {
            columns: 3,
            rows: 2,
        };
        let rects = canvas_overlap_rects(grid, &[1.0, 1.0, 1.0, 1.0]);

        assert_eq!(rects.len(), 4);
        assert_close(rects[2].x + rects[2].width, 3.0);
        assert_close(rects[3].x + rects[3].width, 1.0);
        assert_rect_covers_grid_slot(rects[3], grid, 3);
    }

    #[test]
    fn overlap_rects_preserve_mixed_image_sizes_and_overlap_to_cover_slots() {
        let output = Size {
            width: 3440,
            height: 1440,
        };
        let images = [
            (3440, 1440),
            (1920, 1080),
            (1080, 1920),
            (1000, 1000),
            (3840, 1600),
            (2560, 1440),
            (800, 1200),
            (5120, 1440),
        ];
        let aspects = image_aspects_for_output(&images, output);
        let grid = Grid {
            columns: 4,
            rows: 2,
        };
        let rects = canvas_overlap_rects(grid, &aspects);

        assert_eq!(rects.len(), images.len());
        for (index, rect) in rects.iter().copied().enumerate() {
            assert_rect_covers_grid_slot(rect, grid, index);
            assert_rect_preserves_image_aspect(rect, images[index], output);
            assert_close(rect.width, aspects[index].max(1.0));
            assert_close(rect.height, (1.0 / aspects[index]).max(1.0));
        }

        assert!(rects[2].height > 1.0);
        assert!(rects[7].width > 1.0);
        assert_close(rects[7].width / rects[4].width, aspects[7] / aspects[4]);
    }

    #[test]
    fn overlap_rects_preserve_sizes_for_portrait_outputs() {
        let output = Size {
            width: 1080,
            height: 1920,
        };
        let images = [
            (1080, 1920),
            (1920, 1080),
            (1000, 1000),
            (800, 1200),
            (1440, 3440),
            (3840, 1600),
            (2160, 3840),
            (5120, 1440),
        ];
        let aspects = image_aspects_for_output(&images, output);
        let grid = Grid {
            columns: 2,
            rows: 4,
        };
        let rects = canvas_overlap_rects(grid, &aspects);

        assert_eq!(rects.len(), images.len());
        for (index, rect) in rects.iter().copied().enumerate() {
            assert_rect_covers_grid_slot(rect, grid, index);
            assert_rect_preserves_image_aspect(rect, images[index], output);
            assert_close(rect.width, aspects[index].max(1.0));
            assert_close(rect.height, (1.0 / aspects[index]).max(1.0));
        }

        assert!(rects[1].width > 1.0);
        assert!(rects[4].height > 1.0);
    }

    #[test]
    fn collage_rects_center_focus_images_without_grid_rows() {
        let rects = canvas_collage_rects(
            Grid {
                columns: 5,
                rows: 5,
            },
            &[1.0; 12],
            5,
            8,
            true,
            0.1,
        );

        assert_eq!(rects.len(), 12);
        assert_eq!(rect_center(rects[5]), (0.0, 0.0));
        assert_eq!(rect_center(rects[8]), (3.0, 0.0));
        let (_, supporting_y) = rect_center(rects[6]);
        assert!(
            supporting_y.abs() > 0.01,
            "supporting image should not stay on a grid row"
        );
    }

    #[test]
    fn collage_rects_keep_wide_focus_images_separated() {
        let rects = canvas_collage_rects(
            Grid {
                columns: 5,
                rows: 5,
            },
            &[1.0, 1.0, 1.0, 1.0, 1.0, 4.0, 4.0, 1.0],
            5,
            6,
            true,
            0.1,
        );

        assert!(
            rects[5].x + rects[5].width + 0.17 <= rects[6].x,
            "focus rects should not overlap: old={:?} target={:?}",
            rects[5],
            rects[6]
        );
    }

    #[test]
    fn full_thumbnail_draw_order_keeps_old_on_top_while_zooming_out() {
        let order = canvas_full_thumbnail_draw_order(6, 2, 4, 0.1, 0.3, 0.4);

        assert_eq!(order.last(), Some(&2));
        assert_eq!(order.iter().filter(|index| **index == 2).count(), 1);
        assert_eq!(order.iter().filter(|index| **index == 4).count(), 1);
    }

    #[test]
    fn full_thumbnail_draw_order_keeps_target_on_top_while_zooming_in() {
        let order = canvas_full_thumbnail_draw_order(6, 2, 4, 0.8, 0.3, 0.4);

        assert_eq!(order.last(), Some(&4));
        assert_eq!(order.iter().filter(|index| **index == 2).count(), 1);
        assert_eq!(order.iter().filter(|index| **index == 4).count(), 1);
    }

    #[test]
    fn overlap_overview_centers_edge_tile_and_exposes_background() {
        let rects = canvas_overlap_rects(
            Grid {
                columns: 11,
                rows: 5,
            },
            &[1.0; 55],
        );
        let transform = centered_canvas_overview_transform(rects[0], 0.25);

        assert_close(transform.translate_x, 0.375);
        assert_close(transform.translate_y, 0.375);
    }

    #[test]
    fn expanded_focus_layout_keeps_both_focus_viewports_inside_bounds() {
        let overview_scale = 0.1;
        let mut rects = canvas_collage_rects(
            Grid {
                columns: 5,
                rows: 5,
            },
            &[1.0; 12],
            5,
            8,
            true,
            overview_scale,
        );

        expand_canvas_focus_layout(&mut rects, 5, 8, overview_scale);

        let bounds = canvas_collage_bounds(&rects);
        for index in [5, 8] {
            let center = canvas_rect_center(rects[index]);
            assert!((center.x - bounds.x) * overview_scale >= 0.6199);
            assert!((bounds.x + bounds.width - center.x) * overview_scale >= 0.6199);
            assert!((center.y - bounds.y) * overview_scale >= 0.6199);
            assert!((bounds.y + bounds.height - center.y) * overview_scale >= 0.6199);
        }
    }

    #[test]
    fn final_collage_rect_transform_maps_rect_to_output() {
        let rects = [Rect {
            x: 2.0,
            y: 1.0,
            width: 4.0,
            height: 2.0,
        }];
        let transform = canvas_final_transform(0, &rects);

        assert_close(rects[0].x * transform.scale_x + transform.translate_x, 0.0);
        assert_close(rects[0].y * transform.scale_y + transform.translate_y, 0.0);
        assert_close(
            (rects[0].x + rects[0].width) * transform.scale_x + transform.translate_x,
            1.0,
        );
        assert_close(
            (rects[0].y + rects[0].height) * transform.scale_y + transform.translate_y,
            1.0,
        );
    }

    #[test]
    fn span_slot_rect_maps_to_output_viewport() {
        let span = three_output_span();
        let draw = canvas_wallpaper_rect_for_view(
            canvas_span_slot_rect(&span, 1),
            Some(&span),
            1000.0,
            1000.0,
        );

        assert!(draw.x.abs() < 0.001);
        assert_close(draw.y, 0.0);
        assert_close(draw.width, 1000.0);
        assert_close(draw.height, 1000.0);
    }

    #[test]
    fn span_focus_slot_rect_places_group_tiles_in_monitor_slots() {
        let span = three_output_span();
        let rects = unit_row_rects(9);

        let left = canvas_span_focus_slot_rect(3, 4, &rects, &span).unwrap();
        let middle = canvas_span_focus_slot_rect(4, 4, &rects, &span).unwrap();
        let right = canvas_span_focus_slot_rect(5, 4, &rects, &span).unwrap();

        assert_close(left.x, 3.0);
        assert_close(left.width, 1.0);
        assert_close(middle.x, 4.0);
        assert_close(middle.width, 1.0);
        assert_close(right.x, 5.0);
        assert_close(right.width, 1.0);
    }

    #[test]
    fn span_global_transform_maps_focus_slot_to_output_viewport() {
        let span = three_output_span();
        let rects = unit_row_rects(9);
        let group = canvas_span_group_rect(4, &rects, &span);
        let rect = canvas_span_focus_slot_rect(4, 4, &rects, &span).unwrap();
        let transform = canvas_final_transform_for_rect(group);
        let draw = canvas_draw_rect_for_view(rect, transform, Some(&span), 1000.0, 1000.0);

        assert!(draw.x.abs() < 0.001);
        assert_close(draw.y, 0.0);
        assert_close(draw.width, 1000.0);
        assert_close(draw.height, 1000.0);
    }

    #[test]
    fn span_morph_rect_uses_slot_at_endpoint_and_morph_during_pan() {
        let span = three_output_span();
        let rects = [
            Rect {
                x: 0.0,
                y: 0.0,
                width: 2.0,
                height: 1.0,
            },
            Rect {
                x: 2.0,
                y: 0.0,
                width: 1.0,
                height: 1.0,
            },
            Rect {
                x: 3.0,
                y: 0.0,
                width: 3.0,
                height: 1.0,
            },
        ];
        let endpoint = canvas_span_morph_rect(
            1,
            rects[1],
            1,
            1,
            &rects,
            &span,
            0.0,
            0.3,
            0.4,
            RenderEasing::Linear,
        );
        let pan = canvas_span_morph_rect(
            1,
            rects[1],
            1,
            1,
            &rects,
            &span,
            0.5,
            0.3,
            0.4,
            RenderEasing::Linear,
        );

        assert_close(endpoint.x, 2.0);
        assert_close(endpoint.width, 2.0);
        assert_close(pan.x, rects[1].x);
        assert_close(pan.width, rects[1].width);
    }

    #[test]
    fn span_sticks_surrounding_tiles_to_dynamic_gap_edges() {
        let span = uneven_three_output_span();
        let rects = unit_grid_rects(3, 9);
        let top = canvas_span_morph_rect(
            1,
            rects[1],
            4,
            4,
            &rects,
            &span,
            0.0,
            0.3,
            0.4,
            RenderEasing::Linear,
        );
        let bottom = canvas_span_morph_rect(
            7,
            rects[7],
            4,
            4,
            &rects,
            &span,
            0.0,
            0.3,
            0.4,
            RenderEasing::Linear,
        );

        assert!(top.y > rects[1].y);
        assert!(bottom.y < rects[7].y);
        assert_close(top.y + top.height, 1.1875);
        assert_close(bottom.y, 1.8125);
    }

    #[test]
    fn span_sticky_surrounding_tiles_cover_uneven_output_gaps() {
        let span = uneven_three_output_span();
        let rects = unit_grid_rects(3, 9);
        let drawn_rects = (0..rects.len())
            .map(|index| {
                canvas_span_morph_rect(
                    index,
                    rects[index],
                    4,
                    4,
                    &rects,
                    &span,
                    0.0,
                    0.3,
                    0.4,
                    RenderEasing::Linear,
                )
            })
            .collect::<Vec<_>>();

        let gaps = canvas_gap_rects(canvas_span_group_rect(4, &rects, &span), &drawn_rects);

        assert!(
            gaps.is_empty(),
            "span endpoint should keep the focus group covered by real surrounding tiles: {gaps:?}"
        );
    }

    #[test]
    fn span_sticky_surrounding_tiles_return_to_grid_during_pan() {
        let span = uneven_three_output_span();
        let rects = unit_grid_rects(3, 9);
        let panning = canvas_span_morph_rect(
            7,
            rects[7],
            4,
            4,
            &rects,
            &span,
            0.5,
            0.3,
            0.4,
            RenderEasing::Linear,
        );

        assert_close(panning.x, rects[7].x);
        assert_close(panning.y, rects[7].y);
    }

    #[test]
    fn span_overview_transform_scales_rest_of_canvas_with_focus_page() {
        let span = three_output_span();
        let rects = unit_row_rects(9);
        let old_group = canvas_span_group_rect(4, &rects, &span);
        let old_final = canvas_final_transform_for_rect(old_group);
        let old_overview = centered_canvas_overview_transform(old_group, 0.25);
        let transform = canvas_mode_transform(
            old_final,
            old_overview,
            old_overview,
            old_final,
            0.15,
            0.3,
            0.4,
            RenderEasing::Linear,
        );

        let focus = canvas_rect_apply_transform(rects[4], transform);
        let neighbor = canvas_rect_apply_transform(rects[7], transform);

        assert_close(focus.width, neighbor.width);
        assert!(neighbor.x > focus.x);
    }
}
