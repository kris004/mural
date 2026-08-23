use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use mural_core::world_cache::{
    read_world_cache_snapshot_by_fingerprint, world_lod_tile_cells,
    world_tile_pyramid_cache_entries_for_fingerprint,
};
use mural_ipc::{RenderWorldSetRequest, Response, ScaleMode, SetRequest, WorldRouteFocus};
use mural_render::{WorldLayout, WorldTile, world_tiles_for_route};

use crate::egl_render::WallpaperTexture;
use crate::image_loader;
use crate::transitions::{
    QueuedState, QueuedTransition, QueuedWallpaper, Target, accelerated_duration,
};
use crate::{MuralApp, validate_image_paths};

const MAX_RENDERER_WORLD_ROUTE_TILES: usize = 16;
const MAX_RENDERER_WORLD_REQUEST_TILES: usize = 32;

#[derive(Clone, Copy)]
pub(crate) struct Spec {
    pub(crate) duration: Duration,
    pub(crate) easing: mural_ipc::Easing,
    pub(crate) library_count: usize,
    pub(crate) columns: usize,
    pub(crate) tile_cells: usize,
    pub(crate) route: WorldRouteFocus,
    pub(crate) started_at: Instant,
    pub(crate) accelerated: bool,
}

pub(crate) struct Active {
    pub(crate) easing: mural_ipc::Easing,
    pub(crate) library_count: usize,
    pub(crate) columns: usize,
    pub(crate) tile_cells: usize,
    pub(crate) route: WorldRouteFocus,
    pub(crate) tiles: Vec<WorldTileTexture>,
    pub(crate) accelerated: bool,
}

pub(crate) struct Queued {
    pub(crate) duration: Duration,
    pub(crate) easing: mural_ipc::Easing,
    pub(crate) library_count: usize,
    pub(crate) columns: usize,
    pub(crate) tile_cells: usize,
    pub(crate) route: WorldRouteFocus,
    pub(crate) tiles: Vec<WorldTileTexture>,
}

#[derive(Clone, Copy)]
pub(crate) struct WorldTileTexture {
    pub(crate) lod: usize,
    pub(crate) tile: WorldTile,
    pub(crate) texture: WallpaperTexture,
}

struct WorldUpload {
    surface_index: usize,
    image_path: String,
    texture: WallpaperTexture,
    route: WorldRouteFocus,
    tiles: Vec<WorldTileTexture>,
}

struct WorldQueueUpload {
    surface_index: usize,
    image_path: String,
    route: WorldRouteFocus,
    tiles: Vec<WorldTileTexture>,
}

impl MuralApp {
    pub(crate) fn set_world_wallpapers(&mut self, request: &RenderWorldSetRequest) -> Response {
        let mural_ipc::Transition::World {
            duration_ms,
            easing,
        } = request.transition
        else {
            return Response::Error {
                message: "set_world_wallpapers called with non-world transition".to_owned(),
            };
        };

        if let Err(message) = validate_world_request_cache(&self.config, request) {
            return Response::Error { message };
        }
        if let Err(message) = validate_world_request_tile_budget(request) {
            return Response::Error { message };
        }
        if let Err(message) = validate_image_paths(&request.outputs) {
            return Response::Error { message };
        }
        let set = SetRequest {
            outputs: request.outputs.clone(),
            transition: request.transition,
            scale_mode: request.scale_mode,
            allow_partial: request.allow_partial,
        };
        if let Some(response) = self.skip_set_targets_if_all_power_off(&set) {
            return response;
        }
        if let Err(response) = self.ensure_set_targets_renderable(&set) {
            return response;
        }

        let plan = match self.plan_transition_targets(&set, true) {
            Ok(plan) => plan,
            Err(response) => return response,
        };
        let duration = Duration::from_millis(duration_ms);

        let tile_paths = match world_tile_path_map(&self.config, request) {
            Ok(paths) => paths,
            Err(message) => return Response::Error { message },
        };
        let uploads = match self.upload_world_start_textures(plan.starts, request, &tile_paths) {
            Ok(uploads) => uploads,
            Err(response) => return response,
        };
        let queued_uploads = match self.upload_world_queued_tiles(plan.queued, request, &tile_paths)
        {
            Ok(queued_uploads) => queued_uploads,
            Err(response) => {
                cleanup_world_uploads(&self.egl, uploads);
                return response;
            }
        };
        let queued_count = queued_uploads.len();
        let started = match self.start_world_uploads(
            uploads,
            request.scale_mode,
            Spec {
                duration,
                easing,
                library_count: request.library_count,
                columns: request.columns,
                tile_cells: request.tile_cells,
                route: WorldRouteFocus {
                    current_index: 0,
                    target_index: 0,
                    lod: 0,
                },
                started_at: Instant::now(),
                accelerated: false,
            },
        ) {
            Ok(started) => started,
            Err(response) => {
                cleanup_world_queue_uploads(&self.egl, queued_uploads);
                return response;
            }
        };
        self.enqueue_world_targets(
            queued_uploads,
            request.scale_mode,
            request,
            duration,
            easing,
        );

        Response::Ack {
            message: format!(
                "started {started} output(s), queued {queued_count} output(s) with world transition"
            ),
        }
    }

    fn upload_world_start_textures(
        &mut self,
        starts: Vec<Target>,
        request: &RenderWorldSetRequest,
        tile_paths: &BTreeMap<(usize, usize, usize), PathBuf>,
    ) -> Result<Vec<WorldUpload>, Response> {
        let mut uploads = Vec::with_capacity(starts.len());
        for target in starts {
            match self.upload_world_start_texture(target, request, tile_paths) {
                Ok(upload) => uploads.push(upload),
                Err(response) => {
                    cleanup_world_uploads(&self.egl, uploads);
                    return Err(response);
                }
            }
        }
        Ok(uploads)
    }

    fn upload_world_start_texture(
        &mut self,
        target: Target,
        request: &RenderWorldSetRequest,
        tile_paths: &BTreeMap<(usize, usize, usize), PathBuf>,
    ) -> Result<WorldUpload, Response> {
        let surface_index = target.surface_index;
        let surface_name = target.name.clone();
        let route = world_route_for_target(request, &target.name)?;

        trace_log!(self.trace, "set_world_wallpapers: decode {surface_name}");
        let image = image_loader::load(&target.image_path)
            .map_err(|message| Response::Error { message })?;
        trace_log!(self.trace, "set_world_wallpapers: decoded {surface_name}");

        let texture = self.surfaces[surface_index]
            .upload_wallpaper_texture(&self.egl, &image)
            .map_err(|message| Response::Error { message })?;
        let tiles = match self.upload_world_route_tiles(
            surface_index,
            &surface_name,
            route,
            request,
            tile_paths,
        ) {
            Ok(tiles) => tiles,
            Err(response) => {
                self.egl.delete_texture(texture);
                return Err(response);
            }
        };

        Ok(WorldUpload {
            surface_index,
            image_path: target.image_path,
            texture,
            route,
            tiles,
        })
    }

    fn upload_world_queued_tiles(
        &mut self,
        targets: Vec<Target>,
        request: &RenderWorldSetRequest,
        tile_paths: &BTreeMap<(usize, usize, usize), PathBuf>,
    ) -> Result<Vec<WorldQueueUpload>, Response> {
        let mut uploads = Vec::with_capacity(targets.len());
        for target in targets {
            match self.upload_world_queued_tile(target, request, tile_paths) {
                Ok(upload) => uploads.push(upload),
                Err(response) => {
                    cleanup_world_queue_uploads(&self.egl, uploads);
                    return Err(response);
                }
            }
        }
        Ok(uploads)
    }

    fn upload_world_queued_tile(
        &mut self,
        target: Target,
        request: &RenderWorldSetRequest,
        tile_paths: &BTreeMap<(usize, usize, usize), PathBuf>,
    ) -> Result<WorldQueueUpload, Response> {
        let route = world_route_for_target(request, &target.name)?;
        let surface_index = target.surface_index;
        let surface_name = target.name.clone();
        let tiles = self.upload_world_route_tiles(
            surface_index,
            &surface_name,
            route,
            request,
            tile_paths,
        )?;

        Ok(WorldQueueUpload {
            surface_index,
            image_path: target.image_path,
            route,
            tiles,
        })
    }

    fn upload_world_route_tiles(
        &mut self,
        surface_index: usize,
        surface_name: &str,
        route: WorldRouteFocus,
        request: &RenderWorldSetRequest,
        tile_paths: &BTreeMap<(usize, usize, usize), PathBuf>,
    ) -> Result<Vec<WorldTileTexture>, Response> {
        let route_tiles =
            route_world_tiles(request, route).map_err(|message| Response::Error { message })?;
        let mut tiles = Vec::with_capacity(route_tiles.len());
        for tile in route_tiles {
            let Some(path) = tile_paths.get(&(route.lod, tile.row, tile.column)) else {
                cleanup_world_tiles(&self.egl, tiles);
                return Err(Response::Error {
                    message: format!(
                        "world cache tile is not indexed for LOD {} row {} column {}",
                        route.lod, tile.row, tile.column
                    ),
                });
            };
            if !path.is_file() {
                cleanup_world_tiles(&self.egl, tiles);
                return Err(Response::Error {
                    message: format!("world cache tile is missing: {}", path.display()),
                });
            }
            let decoded = match image_loader::load(path.to_string_lossy().as_ref()) {
                Ok(decoded) => decoded,
                Err(message) => {
                    cleanup_world_tiles(&self.egl, tiles);
                    return Err(Response::Error { message });
                }
            };
            match self.surfaces[surface_index].upload_wallpaper_texture(&self.egl, &decoded) {
                Ok(texture) => tiles.push(WorldTileTexture {
                    lod: route.lod,
                    tile,
                    texture,
                }),
                Err(message) => {
                    cleanup_world_tiles(&self.egl, tiles);
                    return Err(Response::Error { message });
                }
            }
        }
        trace_log!(
            self.trace,
            "set_world_wallpapers: uploaded world tiles {surface_name}"
        );
        Ok(tiles)
    }

    fn start_world_uploads(
        &mut self,
        uploads: Vec<WorldUpload>,
        scale_mode: ScaleMode,
        spec: Spec,
    ) -> Result<usize, Response> {
        let qh = self.qh.clone();
        let started = uploads.len();
        for upload in uploads {
            let surface_index = upload.surface_index;
            self.surfaces[surface_index].start_world_transition(
                &self.egl,
                upload.image_path,
                upload.texture,
                scale_mode,
                upload.tiles,
                Spec {
                    route: upload.route,
                    ..spec
                },
            );
            if let Err(message) = self.render_surface_active(surface_index, &qh) {
                self.surfaces[surface_index].mark_recreate_needed(
                    self.trace,
                    "world first frame",
                    &message,
                );
                return Err(Response::Error { message });
            }
        }
        Ok(started)
    }

    fn enqueue_world_targets(
        &mut self,
        uploads: Vec<WorldQueueUpload>,
        scale_mode: ScaleMode,
        request: &RenderWorldSetRequest,
        duration: Duration,
        easing: mural_ipc::Easing,
    ) {
        for upload in uploads {
            let id = self.next_decode_id();
            self.surfaces[upload.surface_index].enqueue_wallpaper_transition(
                QueuedWallpaper {
                    id,
                    image_path: upload.image_path,
                    scale_mode,
                    transition: QueuedTransition::World(Queued {
                        duration: accelerated_duration(duration),
                        easing,
                        library_count: request.library_count,
                        columns: request.columns,
                        tile_cells: request.tile_cells,
                        route: upload.route,
                        tiles: upload.tiles,
                    }),
                    state: QueuedState::Path,
                },
                &self.decode_tx,
            );
        }
    }
}

fn world_route_for_target(
    request: &RenderWorldSetRequest,
    target_name: &str,
) -> Result<WorldRouteFocus, Response> {
    request
        .routes
        .get(target_name)
        .copied()
        .ok_or_else(|| Response::Error {
            message: format!("world request is missing route metadata for {target_name}"),
        })
}

fn validate_world_request_cache(
    config: &mural_core::MuralConfig,
    request: &RenderWorldSetRequest,
) -> Result<(), String> {
    let snapshot = read_world_cache_snapshot_by_fingerprint(config, request.fingerprint)?
        .ok_or_else(|| {
            format!(
                "world cache snapshot {:016x} is missing; run `muralctl world cache index`",
                request.fingerprint
            )
        })?;
    if snapshot.library_count() != request.library_count
        || snapshot.columns != request.columns
        || snapshot.fingerprint != request.fingerprint
    {
        return Err(
            "world request cache metadata does not match the indexed cache snapshot".to_owned(),
        );
    }
    Ok(())
}

fn world_tile_path_map(
    config: &mural_core::MuralConfig,
    request: &RenderWorldSetRequest,
) -> Result<BTreeMap<(usize, usize, usize), PathBuf>, String> {
    Ok(world_tile_pyramid_cache_entries_for_fingerprint(
        config,
        request.fingerprint,
        request.thumbnail_edge,
        request.tile_cells,
    )?
    .into_iter()
    .map(|entry| {
        (
            (entry.lod, entry.tile_row, entry.tile_column),
            entry.image_path,
        )
    })
    .collect())
}

fn route_world_tiles(
    request: &RenderWorldSetRequest,
    route: WorldRouteFocus,
) -> Result<Vec<WorldTile>, String> {
    let tiles = world_tiles_for_route(
        WorldLayout::new(request.library_count, request.columns),
        route.current_index,
        route.target_index,
        world_lod_tile_cells(request.tile_cells, route.lod),
        1.0,
    );
    if tiles.is_empty() {
        return Err("world renderer route has no cache tiles; check route indices".to_owned());
    }
    if tiles.len() > MAX_RENDERER_WORLD_ROUTE_TILES {
        return Err(format!(
            "world renderer route needs {} tile(s) at LOD {}, exceeding the current safe renderer limit of {MAX_RENDERER_WORLD_ROUTE_TILES}",
            tiles.len(),
            route.lod
        ));
    }
    Ok(tiles)
}

fn validate_world_request_tile_budget(request: &RenderWorldSetRequest) -> Result<(), String> {
    let mut route_tile_uploads = 0_usize;
    for route in request.routes.values().copied() {
        route_tile_uploads =
            route_tile_uploads.saturating_add(route_world_tiles(request, route)?.len());
        if route_tile_uploads > MAX_RENDERER_WORLD_REQUEST_TILES {
            return Err(format!(
                "world renderer request needs {route_tile_uploads} route tile upload(s), exceeding the current safe request limit of {MAX_RENDERER_WORLD_REQUEST_TILES}"
            ));
        }
    }
    Ok(())
}

fn cleanup_world_uploads(egl: &crate::egl_render::EglState, uploads: Vec<WorldUpload>) {
    for upload in uploads {
        egl.delete_texture(upload.texture);
        cleanup_world_tiles(egl, upload.tiles);
    }
}

pub(crate) fn cleanup_queued_world(egl: &crate::egl_render::EglState, queued: Queued) {
    cleanup_world_tiles(egl, queued.tiles);
}

fn cleanup_world_queue_uploads(egl: &crate::egl_render::EglState, uploads: Vec<WorldQueueUpload>) {
    for upload in uploads {
        cleanup_world_tiles(egl, upload.tiles);
    }
}

fn cleanup_world_tiles(egl: &crate::egl_render::EglState, tiles: Vec<WorldTileTexture>) {
    for tile in tiles {
        egl.delete_texture(tile.texture);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mural_core::world_cache::{DEFAULT_WORLD_CELL_THUMBNAIL_EDGE, DEFAULT_WORLD_TILE_CELLS};

    fn request_with_route_count(route_count: usize) -> RenderWorldSetRequest {
        let route = WorldRouteFocus {
            current_index: 0,
            target_index: 399,
            lod: 0,
        };
        RenderWorldSetRequest {
            outputs: BTreeMap::new(),
            transition: mural_ipc::Transition::World {
                duration_ms: 120,
                easing: mural_ipc::Easing::Linear,
            },
            scale_mode: ScaleMode::Fill,
            allow_partial: false,
            library_count: 1_000,
            columns: 40,
            fingerprint: 0,
            thumbnail_edge: DEFAULT_WORLD_CELL_THUMBNAIL_EDGE,
            tile_cells: DEFAULT_WORLD_TILE_CELLS,
            routes: (0..route_count)
                .map(|index| (format!("DP-{index}"), route))
                .collect(),
        }
    }

    #[test]
    fn world_request_tile_budget_accepts_bounded_batches() {
        let request = request_with_route_count(3);
        let tiles = route_world_tiles(&request, request.routes["DP-0"]).unwrap();

        assert_eq!(tiles.len(), 10);
        assert!(validate_world_request_tile_budget(&request).is_ok());
    }

    #[test]
    fn world_request_tile_budget_rejects_aggregate_upload_bursts() {
        let request = request_with_route_count(4);
        let error = validate_world_request_tile_budget(&request).unwrap_err();

        assert!(error.contains("exceeding the current safe request limit"));
        assert!(error.contains("32"));
    }
}
