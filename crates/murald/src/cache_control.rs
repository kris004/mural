use mural_ipc::{
    CacheAction, CacheBackend, CacheResponse, CacheWarmScope, MAX_CANVAS_TILE_COUNT, Response,
    Transition,
};

use crate::MuralApp;
use crate::transitions::canvas::{CanvasCache, CanvasCacheResult, clear_canvas_cache_root};

impl MuralApp {
    pub(crate) fn maybe_warm_canvas_cache(&mut self) {
        if self.flags.cache_warm_done()
            || !self.flags.startup_done()
            || !self.config.uses_canvas_transition()
        {
            return;
        }
        let tile_count = self
            .config
            .canvas_prewarm_transition()
            .and_then(|transition| {
                let Transition::Canvas {
                    pan_axis,
                    overview_scale,
                    tile_count,
                    ..
                } = transition
                else {
                    return None;
                };
                Some(self.resolve_canvas_tile_count(tile_count, overview_scale, pan_axis))
            })
            .unwrap_or(1);
        let required = self
            .surfaces
            .iter()
            .filter_map(|surface| surface.current_image.clone())
            .collect::<Vec<_>>();
        let paths = self.wallpaper.preview_window(&required, tile_count);
        if !paths.is_empty() {
            trace_log!(
                self.trace,
                "canvas cache: scheduling {} background thumbnail(s)",
                paths.len()
            );
            let _scheduled = self.schedule_canvas_cache_warm(
                paths,
                self.config.canvas_cache_workers,
                self.config.canvas_cache_backend,
            );
        }
        self.flags.mark_cache_warm_done();
    }

    pub(crate) fn handle_cache_request(&mut self, request: &mural_ipc::CacheRequest) -> Response {
        match &request.action {
            CacheAction::Status => {
                self.ensure_canvas_cache();
                Response::Cache(self.cache_response("status", 0, None))
            }
            CacheAction::Clear => match self.clear_canvas_cache() {
                Ok(removed) => Response::Cache(self.cache_clear_response(removed)),
                Err(message) => Response::Error { message },
            },
            CacheAction::Warm {
                scope,
                workers,
                backend,
            } => {
                let paths = self.cache_scope_paths(*scope);
                let scheduled = self.schedule_canvas_cache_warm(paths, *workers, *backend);
                Response::Cache(self.cache_response("warm", scheduled, Some(*backend)))
            }
        }
    }

    pub(crate) fn schedule_canvas_cache_warm(
        &mut self,
        paths: Vec<String>,
        workers: usize,
        backend: CacheBackend,
    ) -> usize {
        self.ensure_canvas_cache().schedule(paths, workers, backend)
    }

    pub(crate) fn handle_canvas_cache_result(&mut self, result: &CanvasCacheResult) {
        if let Some(canvas_cache) = &mut self.canvas_cache {
            canvas_cache.accept_result(result);
        }
        if let Err(message) = &result.result {
            eprintln!(
                "murald: failed to cache canvas thumbnail {}: {message}",
                result.source_path
            );
        }
    }

    fn clear_canvas_cache(&mut self) -> Result<usize, String> {
        let root = self.wallpaper.state_dir().join("cache/canvas-v1");
        if let Some(canvas_cache) = &mut self.canvas_cache {
            canvas_cache.clear()
        } else {
            clear_canvas_cache_root(&root)
        }
    }

    fn cache_response(
        &self,
        action: &str,
        scheduled: usize,
        backend: Option<CacheBackend>,
    ) -> CacheResponse {
        let status = self
            .canvas_cache
            .as_ref()
            .map_or_else(CanvasCache::empty_status, CanvasCache::status);
        let message = match action {
            "warm" => format!(
                "ready\t{}\tpending\t{}\tscheduled\t{}\tfailed\t{}",
                status.ready, status.pending, scheduled, status.failed
            ),
            _ => format!(
                "ready\t{}\tpending\t{}\tfailed\t{}",
                status.ready, status.pending, status.failed
            ),
        };
        CacheResponse {
            action: action.to_owned(),
            message,
            ready: status.ready,
            pending: status.pending,
            scheduled,
            failed: status.failed,
            backend: self.canvas_cache_backend_name(backend).to_owned(),
        }
    }

    fn cache_clear_response(&self, removed: usize) -> CacheResponse {
        let status = self
            .canvas_cache
            .as_ref()
            .map_or_else(CanvasCache::empty_status, CanvasCache::status);
        CacheResponse {
            action: "clear".to_owned(),
            message: format!(
                "ready\t{}\tpending\t{}\tremoved\t{}\tfailed\t{}",
                status.ready, status.pending, removed, status.failed
            ),
            ready: status.ready,
            pending: status.pending,
            scheduled: 0,
            failed: status.failed,
            backend: self.canvas_cache_backend_name(None).to_owned(),
        }
    }

    fn cache_scope_paths(&self, scope: CacheWarmScope) -> Vec<String> {
        match scope {
            CacheWarmScope::Current => {
                let required = self
                    .surfaces
                    .iter()
                    .filter_map(|surface| surface.current_image.clone())
                    .collect::<Vec<_>>();
                let tile_count = self
                    .config
                    .canvas_prewarm_transition()
                    .and_then(|transition| {
                        let Transition::Canvas {
                            pan_axis,
                            overview_scale,
                            tile_count,
                            ..
                        } = transition
                        else {
                            return None;
                        };
                        Some(self.resolve_canvas_tile_count(tile_count, overview_scale, pan_axis))
                    })
                    .unwrap_or(MAX_CANVAS_TILE_COUNT.min(12));
                self.wallpaper.preview_window(&required, tile_count)
            }
            CacheWarmScope::All => self.wallpaper.library_paths(),
        }
    }

    fn ensure_canvas_cache(&mut self) -> &mut CanvasCache {
        self.canvas_cache.get_or_insert_with(|| {
            trace_log!(self.trace, "initializing lazy canvas thumbnail cache");
            CanvasCache::new(
                self.wallpaper.state_dir().join("cache/canvas-v1"),
                self.config.canvas_thumbnail_max_edge,
                self.config.canvas_cache_backend,
                self.config.canvas_cache_memory_bytes,
                self.canvas_cache_result_tx.clone(),
            )
        })
    }

    fn canvas_cache_backend_name(&self, backend: Option<CacheBackend>) -> &'static str {
        CanvasCache::effective_backend(backend.unwrap_or(self.config.canvas_cache_backend)).as_str()
    }
}
