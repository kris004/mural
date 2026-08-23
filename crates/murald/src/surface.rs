use std::collections::VecDeque;
use std::mem;
use std::sync::mpsc;
use std::time::Instant;

use khronos_egl as egl;
use mural_ipc::{ScaleMode, Transition, TransitionState};
use mural_render::Size;
use smithay_client_toolkit::reexports::protocols_wlr::output_power_management::v1::client::zwlr_output_power_v1::ZwlrOutputPowerV1;
use smithay_client_toolkit::shell::WaylandSurface as _;
use smithay_client_toolkit::shell::wlr_layer::LayerSurface;
use wayland_client::protocol::wl_output;
use wayland_client::{Proxy as _, QueueHandle};
use wayland_egl::WlEglSurface;

use crate::decode::{DecodeJob, DecodeResult, DecodedImage};
use crate::egl_render::{Color, EglState, WallpaperTexture};
use crate::image_loader;
use crate::transitions::canvas::{
    CanvasCache, CanvasLayoutSpec, CanvasSpan, CanvasTile, CanvasTileArrange, CanvasTileBuild,
    accelerated_canvas_phases, arrange_canvas_tile_paths, canvas_path_index,
    canvas_ready_tile_count, canvas_tile_paths, ensure_canvas_path,
    resolve_canvas_tile_count_for_pan,
};
use crate::transitions::{
    ActiveTransition, ActiveTransitionKind, QueuedState, QueuedTransition, QueuedWallpaper,
    canvas as canvas_transition, delete_canvas_tiles, delete_transition_aux_textures,
    pairwise as pairwise_transition, world as world_transition,
};
use crate::{MAX_PREPARED_PER_OUTPUT, MIN_CANVAS_READY_TILES, MuralApp, TraceMode};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum OutputPowerState {
    Unknown,
    On,
    Off,
    Unsupported,
}

impl OutputPowerState {
    // Central policy gate for OutputSurface EGL work. Transition code should
    // route surface resize, texture upload, frame rendering, and queued uploads
    // through OutputSurface instead of checking output power itself.
    pub(crate) const fn allows_egl(self) -> bool {
        matches!(self, Self::On | Self::Unsupported)
    }

    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::On => "on",
            Self::Off => "off",
            Self::Unsupported => "unsupported",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum EglLifecycleState {
    Normal,
    RecreateNeeded,
}

pub(crate) struct OutputSurface {
    pub(crate) output: wl_output::WlOutput,
    pub(crate) name: String,
    pub(crate) layout_x: i32,
    pub(crate) layout_y: i32,
    pub(crate) output_power: Option<ZwlrOutputPowerV1>,
    pub(crate) power_state: OutputPowerState,
    // Drop order matters: WlEglSurface must be destroyed before LayerSurface
    // drops the underlying wl_surface.
    pub(crate) egl_window: Option<WlEglSurface>,
    pub(crate) egl_surface: Option<egl::Surface>,
    pub(crate) layer: LayerSurface,
    pub(crate) width: i32,
    pub(crate) height: i32,
    pub(crate) current_image: Option<String>,
    pub(crate) scale_mode: ScaleMode,
    pub(crate) clear_color: Color,
    pub(crate) wallpaper: Option<WallpaperTexture>,
    pub(crate) transition: Option<ActiveTransition>,
    pub(crate) queue: VecDeque<QueuedWallpaper>,
    pub(crate) frame_callback_pending: bool,
    pub(crate) restore_pending: bool,
    pub(crate) render_pending: bool,
    pub(crate) egl_lifecycle: EglLifecycleState,
}

struct QueuedStart {
    image_path: String,
    scale_mode: ScaleMode,
    texture: WallpaperTexture,
}

impl OutputSurface {
    pub(crate) const fn egl_ready(&self) -> bool {
        self.power_state.allows_egl()
    }

    pub(crate) fn defer_egl_operation(&mut self, trace: TraceMode, operation: &str) -> bool {
        if self.egl_ready() {
            return false;
        }

        self.defer_render(trace, operation);
        true
    }

    pub(crate) fn defer_configure(&mut self, trace: TraceMode, width: i32, height: i32) -> bool {
        if self.egl_ready() {
            return false;
        }

        self.width = width;
        self.height = height;
        self.defer_render(trace, "configure_layer");
        true
    }

    fn egl_deferred_error(&self, operation: &str) -> String {
        format!(
            "{operation} deferred for output {} while output power is {}",
            self.name,
            self.power_state.name()
        )
    }

    pub(crate) fn ensure_egl_surface(
        &mut self,
        egl_state: &EglState,
        width: i32,
        height: i32,
    ) -> Result<(), String> {
        if !self.egl_ready() {
            return Err(self.egl_deferred_error("EGL surface creation"));
        }
        if self.width == width
            && self.height == height
            && self.egl_surface.is_some()
            && self.egl_lifecycle == EglLifecycleState::Normal
        {
            return Ok(());
        }

        self.destroy_egl_surface(egl_state);

        let egl_window = WlEglSurface::new(self.layer.wl_surface().id(), width, height)
            .map_err(|error| format!("failed to create Wayland EGL window: {error}"))?;
        let egl_surface = egl_state.create_window_surface(&egl_window)?;
        egl_state.configure_swap_interval(egl_surface)?;

        self.width = width;
        self.height = height;
        self.egl_window = Some(egl_window);
        self.egl_surface = Some(egl_surface);
        self.egl_lifecycle = EglLifecycleState::Normal;
        Ok(())
    }

    fn render_egl_surface(&mut self, egl_state: &EglState) -> Result<Option<egl::Surface>, String> {
        if self.width <= 0 || self.height <= 0 {
            return Ok(None);
        }

        self.ensure_egl_surface(egl_state, self.width, self.height)?;
        Ok(self.egl_surface)
    }

    pub(crate) fn render_clear(
        &mut self,
        egl_state: &EglState,
        trace: TraceMode,
    ) -> Result<(), String> {
        if self.defer_egl_operation(trace, "render_clear") {
            return Ok(());
        }
        let Some(egl_surface) = self.render_egl_surface(egl_state)? else {
            return Ok(());
        };

        trace_log!(trace, "render_clear {}: draw", self.name);
        egl_state.render_clear(egl_surface, self.width, self.height, self.clear_color)?;
        trace_log!(trace, "render_clear {}: damage/swap", self.name);
        self.layer
            .wl_surface()
            .damage_buffer(0, 0, self.width, self.height);
        egl_state.swap_buffers(egl_surface)?;
        trace_log!(trace, "render_clear {}: swapped", self.name);
        self.render_pending = false;
        Ok(())
    }

    pub(crate) fn render_current(
        &mut self,
        egl_state: &EglState,
        trace: TraceMode,
    ) -> Result<(), String> {
        let Some(wallpaper) = self.wallpaper else {
            return self.render_clear(egl_state, trace);
        };
        if self.defer_egl_operation(trace, "render_current") {
            return Ok(());
        }
        let Some(egl_surface) = self.render_egl_surface(egl_state)? else {
            return Ok(());
        };

        trace_log!(trace, "render_current {}: draw", self.name);
        egl_state.render_wallpaper(
            egl_surface,
            self.width,
            self.height,
            wallpaper,
            self.scale_mode,
            self.clear_color,
        )?;
        trace_log!(trace, "render_current {}: damage/swap", self.name);
        self.layer
            .wl_surface()
            .damage_buffer(0, 0, self.width, self.height);
        egl_state.swap_buffers(egl_surface)?;
        trace_log!(trace, "render_current {}: swapped", self.name);
        self.render_pending = false;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn render_active(
        &mut self,
        egl_state: &EglState,
        qh: &QueueHandle<MuralApp>,
        decode_tx: &mpsc::Sender<DecodeJob>,
        canvas_cache: &mut Option<CanvasCache>,
        pan_tiles: usize,
        span: Option<CanvasSpan>,
        trace: TraceMode,
    ) -> Result<(), String> {
        if self.transition.is_some() {
            self.render_transition_frame(
                egl_state,
                qh,
                decode_tx,
                canvas_cache,
                pan_tiles,
                span,
                trace,
            )
        } else {
            self.render_current(egl_state, trace)
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn render_transition_frame(
        &mut self,
        egl_state: &EglState,
        qh: &QueueHandle<MuralApp>,
        decode_tx: &mpsc::Sender<DecodeJob>,
        canvas_cache: &mut Option<CanvasCache>,
        pan_tiles: usize,
        span: Option<CanvasSpan>,
        trace: TraceMode,
    ) -> Result<(), String> {
        if self.transition.is_none() {
            return self.render_current(egl_state, trace);
        }

        if self.defer_egl_operation(trace, "render_transition_frame") {
            return Ok(());
        }

        let progress = self
            .transition
            .as_ref()
            .map_or(0.0, ActiveTransition::render_progress);
        if progress >= 1.0 {
            return self.finish_transition_frame(
                egl_state,
                qh,
                decode_tx,
                canvas_cache,
                pan_tiles,
                span,
                trace,
            );
        }

        let Some(egl_surface) = self.render_egl_surface(egl_state)? else {
            return Ok(());
        };

        let Some(transition) = self.transition.as_ref() else {
            return self.render_current(egl_state, trace);
        };

        trace_frame_log!(
            trace,
            "render_transition_frame {}: draw progress={progress:.3}",
            self.name
        );
        transition.render_frame(
            egl_state,
            egl_surface,
            self.width,
            self.height,
            self.clear_color,
            progress,
            span,
        )?;
        self.swap_transition_frame(egl_state, qh, egl_surface, trace)
    }

    #[allow(clippy::too_many_arguments)]
    fn finish_transition_frame(
        &mut self,
        egl_state: &EglState,
        qh: &QueueHandle<MuralApp>,
        decode_tx: &mpsc::Sender<DecodeJob>,
        canvas_cache: &mut Option<CanvasCache>,
        pan_tiles: usize,
        span: Option<CanvasSpan>,
        trace: TraceMode,
    ) -> Result<(), String> {
        self.settle_transition(egl_state);
        if self.start_next_queued(egl_state, decode_tx, canvas_cache, pan_tiles) {
            return self.render_transition_frame(
                egl_state,
                qh,
                decode_tx,
                canvas_cache,
                pan_tiles,
                span,
                trace,
            );
        }
        self.render_current(egl_state, trace)
    }

    fn swap_transition_frame(
        &mut self,
        egl_state: &EglState,
        qh: &QueueHandle<MuralApp>,
        egl_surface: egl::Surface,
        trace: TraceMode,
    ) -> Result<(), String> {
        trace_frame_log!(trace, "render_transition_frame {}: frame/damage", self.name);
        self.layer
            .wl_surface()
            .damage_buffer(0, 0, self.width, self.height);
        self.request_frame(qh);
        trace_frame_log!(trace, "render_transition_frame {}: swap", self.name);
        egl_state.swap_buffers(egl_surface)?;
        trace_frame_log!(trace, "render_transition_frame {}: swapped", self.name);
        self.render_pending = false;
        Ok(())
    }

    pub(crate) fn defer_render(&mut self, trace: TraceMode, operation: &str) {
        self.render_pending = true;
        trace_log!(
            trace,
            "{operation} {}: deferred while output power is {}",
            self.name,
            self.power_state.name()
        );
    }

    pub(crate) fn mark_recreate_needed(&mut self, trace: TraceMode, operation: &str, error: &str) {
        self.egl_lifecycle = EglLifecycleState::RecreateNeeded;
        self.render_pending = true;
        eprintln!(
            "murald: marking EGL surface for {} recreate after {operation} failed: {error}",
            self.name
        );
        trace_log!(
            trace,
            "{operation} {}: marked recreate-needed after error",
            self.name
        );
    }

    pub(crate) fn render_state_name(&self) -> &'static str {
        if !self.power_state.allows_egl() {
            return match self.power_state {
                OutputPowerState::Unknown => "power-unknown",
                OutputPowerState::Off => "power-off",
                OutputPowerState::On | OutputPowerState::Unsupported => unreachable!(),
            };
        }
        if self.width <= 0 || self.height <= 0 {
            return "unconfigured";
        }
        if self.egl_lifecycle == EglLifecycleState::RecreateNeeded {
            return "recreate-needed";
        }
        if self.egl_surface.is_none() {
            return "configured-not-egl-ready";
        }
        if self.frame_callback_pending && self.transition.is_some() {
            return "waiting-frame";
        }
        "renderable"
    }

    pub(crate) fn request_frame(&mut self, qh: &QueueHandle<MuralApp>) {
        if self.frame_callback_pending {
            return;
        }

        self.layer
            .wl_surface()
            .frame(qh, self.layer.wl_surface().clone());
        self.frame_callback_pending = true;
    }

    pub(crate) fn set_cut_wallpaper(
        &mut self,
        egl_state: &EglState,
        image_path: String,
        texture: WallpaperTexture,
        scale_mode: ScaleMode,
    ) {
        self.clear_queue(egl_state);
        self.settle_transition(egl_state);
        if let Some(old_texture) = self.wallpaper.replace(texture) {
            egl_state.delete_texture(old_texture);
        }
        self.current_image = Some(image_path);
        self.scale_mode = scale_mode;
        self.restore_pending = false;
    }

    pub(crate) fn enqueue_wallpaper_transition(
        &mut self,
        queued: QueuedWallpaper,
        decode_tx: &mpsc::Sender<DecodeJob>,
    ) {
        if let Some(transition) = &mut self.transition {
            transition.accelerate_remaining();
        }
        self.restore_pending = false;
        self.queue.push_back(queued);
        self.schedule_decode_lookahead(decode_tx);
    }

    pub(crate) fn start_pairwise_transition(
        &mut self,
        egl_state: &EglState,
        image_path: String,
        texture: WallpaperTexture,
        scale_mode: ScaleMode,
        spec: pairwise_transition::Spec,
    ) {
        self.settle_transition(egl_state);
        let old = self.wallpaper.take();
        self.restore_pending = false;
        self.transition = Some(ActiveTransition {
            old,
            old_scale_mode: self.scale_mode,
            new: Some(texture),
            new_image: image_path,
            new_scale_mode: scale_mode,
            transition: spec.effect.transition(spec.duration),
            started_at: spec.started_at,
            duration: spec.duration,
            kind: ActiveTransitionKind::Pairwise(pairwise_transition::Active {
                effect: spec.effect,
                accelerated: spec.accelerated,
            }),
        });
    }

    pub(crate) fn start_world_transition(
        &mut self,
        egl_state: &EglState,
        image_path: String,
        texture: WallpaperTexture,
        scale_mode: ScaleMode,
        tiles: Vec<world_transition::WorldTileTexture>,
        spec: world_transition::Spec,
    ) {
        self.settle_transition(egl_state);
        let old = self.wallpaper.take();
        self.restore_pending = false;
        self.transition = Some(ActiveTransition {
            old,
            old_scale_mode: self.scale_mode,
            new: Some(texture),
            new_image: image_path,
            new_scale_mode: scale_mode,
            transition: Transition::World {
                duration_ms: spec.duration.as_millis().try_into().unwrap_or(u64::MAX),
                easing: spec.easing,
            },
            started_at: spec.started_at,
            duration: spec.duration,
            kind: ActiveTransitionKind::World(world_transition::Active {
                easing: spec.easing,
                library_count: spec.library_count,
                columns: spec.columns,
                tile_cells: spec.tile_cells,
                route: spec.route,
                tiles,
                accelerated: spec.accelerated,
            }),
        });
    }

    pub(crate) fn start_canvas_transition(
        &mut self,
        egl_state: &EglState,
        image_path: String,
        texture: WallpaperTexture,
        scale_mode: ScaleMode,
        mut tiles: Vec<CanvasTile>,
        spec: canvas_transition::Spec,
    ) {
        self.settle_transition(egl_state);
        let old_image = self.current_image.clone();
        if let Some(old_image) = &old_image {
            ensure_canvas_path(&mut tiles, old_image);
        }
        ensure_canvas_path(&mut tiles, &image_path);
        let target_index = canvas_path_index(&tiles, &image_path).unwrap_or(0);
        let old_index = old_image
            .as_deref()
            .and_then(|path| canvas_path_index(&tiles, path))
            .unwrap_or(target_index);
        let old = self.wallpaper.take();
        self.restore_pending = false;
        self.transition = Some(ActiveTransition {
            old,
            old_scale_mode: self.scale_mode,
            new: Some(texture),
            new_image: image_path,
            new_scale_mode: scale_mode,
            transition: Transition::Canvas {
                zoom_out_ms: spec.zoom_out_ms,
                pan_ms: spec.pan_ms,
                zoom_in_ms: spec.zoom_in_ms,
                easing: spec.easing,
                mode: spec.mode,
                walk: spec.walk,
                pan_axis: spec.pan_axis,
                overview_scale: spec.overview_scale,
                tile_count: spec.tile_count,
            },
            started_at: spec.started_at,
            duration: spec.duration(),
            kind: ActiveTransitionKind::Canvas(canvas_transition::Active {
                easing: spec.easing,
                tiles,
                old_index,
                target_index,
                zoom_out_ms: spec.zoom_out_ms,
                pan_ms: spec.pan_ms,
                zoom_in_ms: spec.zoom_in_ms,
                mode: spec.mode,
                pan_axis: spec.pan_axis,
                overview_scale: spec.overview_scale,
                target_decode_id: None,
                accelerated: spec.accelerated,
            }),
        });
    }

    pub(crate) fn start_pending_canvas_transition(
        &mut self,
        egl_state: &EglState,
        image_path: String,
        scale_mode: ScaleMode,
        mut tiles: Vec<CanvasTile>,
        spec: canvas_transition::Spec,
        decode_id: u64,
    ) {
        self.settle_transition(egl_state);
        let old_image = self.current_image.clone();
        if let Some(old_image) = &old_image {
            ensure_canvas_path(&mut tiles, old_image);
        }
        ensure_canvas_path(&mut tiles, &image_path);
        let target_index = canvas_path_index(&tiles, &image_path).unwrap_or(0);
        let old_index = old_image
            .as_deref()
            .and_then(|path| canvas_path_index(&tiles, path))
            .unwrap_or(target_index);
        let old = self.wallpaper.take();
        self.restore_pending = false;
        self.transition = Some(ActiveTransition {
            old,
            old_scale_mode: self.scale_mode,
            new: None,
            new_image: image_path,
            new_scale_mode: scale_mode,
            transition: Transition::Canvas {
                zoom_out_ms: spec.zoom_out_ms,
                pan_ms: spec.pan_ms,
                zoom_in_ms: spec.zoom_in_ms,
                easing: spec.easing,
                mode: spec.mode,
                walk: spec.walk,
                pan_axis: spec.pan_axis,
                overview_scale: spec.overview_scale,
                tile_count: spec.tile_count,
            },
            started_at: spec.started_at,
            duration: spec.duration(),
            kind: ActiveTransitionKind::Canvas(canvas_transition::Active {
                easing: spec.easing,
                tiles,
                old_index,
                target_index,
                zoom_out_ms: spec.zoom_out_ms,
                pan_ms: spec.pan_ms,
                zoom_in_ms: spec.zoom_in_ms,
                mode: spec.mode,
                pan_axis: spec.pan_axis,
                overview_scale: spec.overview_scale,
                target_decode_id: Some(decode_id),
                accelerated: spec.accelerated,
            }),
        });
    }

    pub(crate) fn start_next_queued(
        &mut self,
        egl_state: &EglState,
        decode_tx: &mpsc::Sender<DecodeJob>,
        canvas_cache: &mut Option<CanvasCache>,
        pan_tiles: usize,
    ) -> bool {
        while let Some(queued) = self.queue.pop_front() {
            self.schedule_decode_lookahead(decode_tx);
            let texture = match self.texture_for_queued(egl_state, &queued) {
                Ok(texture) => texture,
                Err(message) => {
                    eprintln!(
                        "murald: skipping queued wallpaper for {}: {message}",
                        self.name
                    );
                    delete_queued_transition_aux_textures(egl_state, queued.transition);
                    continue;
                }
            };

            self.start_queued_wallpaper(egl_state, canvas_cache, pan_tiles, queued, texture);
            return true;
        }

        false
    }

    fn start_queued_wallpaper(
        &mut self,
        egl_state: &EglState,
        canvas_cache: &mut Option<CanvasCache>,
        pan_tiles: usize,
        queued: QueuedWallpaper,
        texture: WallpaperTexture,
    ) {
        let QueuedWallpaper {
            image_path,
            scale_mode,
            transition,
            ..
        } = queued;
        match transition {
            QueuedTransition::Pairwise(pairwise) => self.start_pairwise_transition(
                egl_state,
                image_path,
                texture,
                scale_mode,
                pairwise_transition::Spec {
                    effect: pairwise.effect,
                    duration: pairwise.duration,
                    started_at: Instant::now(),
                    accelerated: true,
                },
            ),
            QueuedTransition::Canvas(canvas) => {
                self.start_queued_canvas(
                    egl_state,
                    canvas_cache,
                    pan_tiles,
                    QueuedStart {
                        image_path,
                        scale_mode,
                        texture,
                    },
                    &canvas,
                );
            }
            QueuedTransition::World(world) => self.start_world_transition(
                egl_state,
                image_path,
                texture,
                scale_mode,
                world.tiles,
                world_transition::Spec {
                    duration: world.duration,
                    easing: world.easing,
                    library_count: world.library_count,
                    columns: world.columns,
                    tile_cells: world.tile_cells,
                    route: world.route,
                    started_at: Instant::now(),
                    accelerated: true,
                },
            ),
        }
    }

    fn start_queued_canvas(
        &mut self,
        egl_state: &EglState,
        canvas_cache: &mut Option<CanvasCache>,
        pan_tiles: usize,
        start: QueuedStart,
        canvas: &canvas_transition::Queued,
    ) {
        let (zoom_out_ms, pan_ms, zoom_in_ms) =
            accelerated_canvas_phases(canvas.zoom_out_ms, canvas.pan_ms, canvas.zoom_in_ms);
        let old_path = self.current_image.clone();
        let tiles = self.canvas_tiles_for_preview_paths(
            egl_state,
            canvas_cache,
            CanvasTileBuild {
                preview_paths: &canvas.preview_paths,
                preview_start: canvas.preview_start,
                old_path: old_path.as_deref(),
                target_path: &start.image_path,
                layout: CanvasLayoutSpec {
                    tile_count: canvas.tile_count,
                    mode: canvas.mode,
                    walk: canvas.walk,
                    pan_axis: canvas.pan_axis,
                    overview_scale: canvas.overview_scale,
                },
                pan_tiles,
            },
        );
        let ready_tiles = canvas_ready_tile_count(&tiles, old_path.as_deref(), &start.image_path);
        if ready_tiles < MIN_CANVAS_READY_TILES {
            delete_canvas_tiles(egl_state, tiles);
            self.set_cut_wallpaper(egl_state, start.image_path, start.texture, start.scale_mode);
        } else {
            self.start_canvas_transition(
                egl_state,
                start.image_path,
                start.texture,
                start.scale_mode,
                tiles,
                canvas_transition::Spec {
                    zoom_out_ms,
                    pan_ms,
                    zoom_in_ms,
                    easing: canvas.easing,
                    mode: canvas.mode,
                    walk: canvas.walk,
                    pan_axis: canvas.pan_axis,
                    overview_scale: canvas.overview_scale,
                    tile_count: canvas.tile_count,
                    started_at: Instant::now(),
                    accelerated: true,
                },
            );
        }
    }

    pub(crate) fn texture_for_queued(
        &self,
        egl_state: &EglState,
        queued: &QueuedWallpaper,
    ) -> Result<WallpaperTexture, String> {
        match &queued.state {
            QueuedState::Uploaded(texture) => Ok(*texture),
            QueuedState::Decoded(image) => self.upload_wallpaper_texture(egl_state, image),
            QueuedState::Failed(message) => Err(message.clone()),
            QueuedState::Path | QueuedState::Decoding => {
                let image = image_loader::load(&queued.image_path)?;
                self.upload_wallpaper_texture(egl_state, &image)
            }
        }
    }

    pub(crate) fn schedule_decode_lookahead(&mut self, decode_tx: &mpsc::Sender<DecodeJob>) {
        let mut prepared = self
            .queue
            .iter()
            .take(MAX_PREPARED_PER_OUTPUT)
            .filter(|queued| {
                matches!(
                    queued.state,
                    QueuedState::Decoding | QueuedState::Decoded(_) | QueuedState::Uploaded(_)
                )
            })
            .count();

        for queued in self.queue.iter_mut().take(MAX_PREPARED_PER_OUTPUT) {
            if prepared >= MAX_PREPARED_PER_OUTPUT {
                break;
            }
            if !matches!(queued.state, QueuedState::Path) {
                continue;
            }

            let job = DecodeJob {
                id: queued.id,
                image_path: queued.image_path.clone(),
            };
            queued.state = QueuedState::Decoding;
            match decode_tx.send(job) {
                Ok(()) => prepared += 1,
                Err(error) => {
                    queued.state =
                        QueuedState::Failed(format!("decode worker is unavailable: {error}"));
                }
            }
        }
    }

    pub(crate) fn has_queued_decode(&self, id: u64) -> bool {
        self.queue.iter().any(|queued| queued.id == id)
    }

    pub(crate) fn accept_decode_result(&mut self, result: DecodeResult) {
        let Some(queued) = self.queue.iter_mut().find(|queued| queued.id == result.id) else {
            return;
        };

        queued.state = match result.result {
            Ok(image) => QueuedState::Decoded(image),
            Err(message) => QueuedState::Failed(message),
        };
    }

    pub(crate) fn has_active_canvas_decode(&self, id: u64) -> bool {
        let Some(transition) = &self.transition else {
            return false;
        };
        matches!(
            &transition.kind,
            ActiveTransitionKind::Canvas(canvas)
                if canvas.target_decode_id.is_some_and(|decode_id| decode_id == id)
        )
    }

    pub(crate) fn accept_active_canvas_decode(
        &mut self,
        egl_state: &EglState,
        result: DecodeResult,
    ) -> Result<bool, String> {
        if !self.has_active_canvas_decode(result.id) {
            return Ok(false);
        }

        let image = match result.result {
            Ok(image) => image,
            Err(message) => {
                self.restore_old_transition_wallpaper(egl_state);
                return Err(message);
            }
        };
        let texture = match self.upload_wallpaper_texture(egl_state, &image) {
            Ok(texture) => texture,
            Err(message) => {
                self.restore_old_transition_wallpaper(egl_state);
                return Err(message);
            }
        };

        let Some(transition) = &mut self.transition else {
            egl_state.delete_texture(texture);
            return Ok(false);
        };
        let ActiveTransitionKind::Canvas(canvas) = &mut transition.kind else {
            egl_state.delete_texture(texture);
            return Ok(false);
        };

        if let Some(old_texture) = transition.new.replace(texture) {
            egl_state.delete_texture(old_texture);
        }
        canvas.target_decode_id = None;
        transition.retime_canvas_zoom_in_if_waiting();
        Ok(true)
    }

    pub(crate) fn upload_one_queued_texture(&mut self, egl_state: &EglState) -> bool {
        if !self.egl_ready() {
            return false;
        }

        let Some(index) = self
            .queue
            .iter()
            .position(|queued| matches!(queued.state, QueuedState::Decoded(_)))
        else {
            return false;
        };

        let QueuedState::Decoded(image) =
            mem::replace(&mut self.queue[index].state, QueuedState::Path)
        else {
            return false;
        };

        self.queue[index].state = match self.upload_wallpaper_texture(egl_state, &image) {
            Ok(texture) => QueuedState::Uploaded(texture),
            Err(message) => QueuedState::Failed(message),
        };
        true
    }

    pub(crate) fn clear_queue(&mut self, egl_state: &EglState) {
        while let Some(queued) = self.queue.pop_front() {
            if let QueuedState::Uploaded(texture) = queued.state {
                egl_state.delete_texture(texture);
            }
            delete_queued_transition_aux_textures(egl_state, queued.transition);
        }
    }

    pub(crate) fn settle_transition(&mut self, egl_state: &EglState) {
        let Some(transition) = self.transition.take() else {
            return;
        };

        delete_transition_aux_textures(egl_state, transition.kind);
        if let Some(new) = transition.new {
            if let Some(old) = transition.old {
                egl_state.delete_texture(old);
            }
            self.wallpaper = Some(new);
            self.current_image = Some(transition.new_image);
            self.scale_mode = transition.new_scale_mode;
        } else if let Some(old) = transition.old {
            self.wallpaper = Some(old);
            self.scale_mode = transition.old_scale_mode;
        }
    }

    pub(crate) fn restore_old_transition_wallpaper(&mut self, egl_state: &EglState) {
        let Some(transition) = self.transition.take() else {
            return;
        };

        delete_transition_aux_textures(egl_state, transition.kind);
        if let Some(new) = transition.new {
            egl_state.delete_texture(new);
        }
        if let Some(old) = transition.old {
            self.wallpaper = Some(old);
            self.scale_mode = transition.old_scale_mode;
        }
    }

    pub(crate) fn discard_transition(&mut self, egl_state: &EglState) {
        let Some(transition) = self.transition.take() else {
            return;
        };

        if let Some(old) = transition.old {
            egl_state.delete_texture(old);
        }
        delete_transition_aux_textures(egl_state, transition.kind);
        if let Some(new) = transition.new {
            egl_state.delete_texture(new);
        }
    }

    pub(crate) fn transition_state(&self) -> TransitionState {
        self.transition
            .as_ref()
            .map_or(TransitionState::Idle, |transition| {
                TransitionState::Running {
                    transition: transition.transition,
                }
            })
    }

    pub(crate) fn upload_wallpaper_texture(
        &self,
        egl_state: &EglState,
        image: &DecodedImage,
    ) -> Result<WallpaperTexture, String> {
        if !self.egl_ready() {
            return Err(self.egl_deferred_error("EGL texture upload"));
        }
        let Some(egl_surface) = self.egl_surface else {
            return Err(format!(
                "output {} is not ready for EGL rendering yet",
                self.name
            ));
        };

        egl_state.upload_texture(egl_surface, image)
    }

    pub(crate) fn canvas_tiles_for_preview_paths(
        &mut self,
        egl_state: &EglState,
        canvas_cache: &mut Option<CanvasCache>,
        spec: CanvasTileBuild<'_>,
    ) -> Vec<CanvasTile> {
        let output = surface_size(self).unwrap_or(Size {
            width: 1920,
            height: 1080,
        });
        let tile_count = resolve_canvas_tile_count_for_pan(
            spec.layout.tile_count,
            spec.layout.overview_scale,
            output,
            spec.pan_tiles,
            spec.layout.pan_axis,
        );
        let paths = arrange_canvas_tile_paths(
            canvas_tile_paths(
                spec.preview_paths,
                spec.old_path,
                spec.target_path,
                tile_count,
            ),
            CanvasTileArrange {
                preview_start: spec.preview_start,
                old_path: spec.old_path,
                target_path: spec.target_path,
                layout: spec.layout,
                output,
                pan_tiles: spec.pan_tiles,
            },
        );
        let mut tiles = Vec::with_capacity(paths.len());
        for path in paths {
            let texture = if Some(path.as_str()) == spec.old_path {
                None
            } else {
                let image = canvas_cache
                    .as_mut()
                    .and_then(|canvas_cache| canvas_cache.get_image(&path));
                image.and_then(
                    |image| match self.upload_wallpaper_texture(egl_state, &image) {
                        Ok(texture) => Some(texture),
                        Err(message) => {
                            eprintln!(
                                "murald: failed to upload canvas thumbnail {path}: {message}"
                            );
                            None
                        }
                    },
                )
            };
            tiles.push(CanvasTile { path, texture });
        }
        tiles
    }

    pub(crate) fn destroy(&mut self, egl_state: &EglState) {
        if let Some(power) = self.output_power.take() {
            power.destroy();
        }
        self.clear_queue(egl_state);
        self.discard_transition(egl_state);
        if let Some(texture) = self.wallpaper.take() {
            egl_state.delete_texture(texture);
        }
        self.destroy_egl_surface(egl_state);
    }

    pub(crate) fn destroy_egl_surface(&mut self, egl_state: &EglState) {
        if let Some(egl_surface) = self.egl_surface.take()
            && let Err(error) = egl_state.destroy_surface(egl_surface)
        {
            eprintln!(
                "murald: failed to destroy EGL surface for {}: {error}",
                self.name
            );
        }
        self.egl_window.take();
        self.width = 0;
        self.height = 0;
        self.egl_lifecycle = EglLifecycleState::Normal;
    }
}

fn delete_queued_transition_aux_textures(egl_state: &EglState, transition: QueuedTransition) {
    match transition {
        QueuedTransition::Pairwise(_) | QueuedTransition::Canvas(_) => {}
        QueuedTransition::World(world) => world_transition::cleanup_queued_world(egl_state, world),
    }
}

pub(crate) fn surface_size(surface: &OutputSurface) -> Option<Size> {
    let width = u32::try_from(surface.width).ok()?;
    let height = u32::try_from(surface.height).ok()?;
    if width == 0 || height == 0 {
        return None;
    }
    Some(Size { width, height })
}

#[cfg(test)]
mod tests {
    use super::OutputPowerState;

    #[test]
    fn egl_work_waits_for_known_power_on() {
        assert!(!OutputPowerState::Unknown.allows_egl());
        assert!(!OutputPowerState::Off.allows_egl());
        assert!(OutputPowerState::On.allows_egl());
        assert!(OutputPowerState::Unsupported.allows_egl());
    }
}
