use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use crate::MuralConfig;
use crate::wallpaper::{configured_wallpaper_paths, scan_top_level_library};

pub const WORLD_CACHE_VERSION: u32 = 1;
pub const WORLD_ORDER_POLICY: &str = "path-snapshot-v1";
pub const DEFAULT_WORLD_CELL_THUMBNAIL_EDGE: u32 = 384;
pub const DEFAULT_WORLD_TILE_CELLS: usize = 8;
pub const WORLD_TILE_LOD_BRANCHING: usize = 8;
const WORLD_CELL_CACHE_ALGORITHM: u32 = 2;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorldCacheStatus {
    pub wall_dir: PathBuf,
    pub state_dir: PathBuf,
    pub cache_dir: PathBuf,
    pub manifest_path: PathBuf,
    pub library_count: usize,
    pub columns: usize,
    pub rows: usize,
    pub fingerprint: u64,
    pub order_policy: String,
    pub thumbnail_edge: u32,
    pub cell_ready: usize,
    pub cell_missing: usize,
    pub world_tile_ready: usize,
    pub world_tile_missing: usize,
    pub world_lods: Vec<WorldLodCacheStatus>,
    pub manifest_state: ManifestState,
    pub ready: bool,
    pub message: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorldLodCacheStatus {
    pub lod: usize,
    pub tile_ready: usize,
    pub tile_missing: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ManifestState {
    Missing,
    Current,
    Stale,
    Invalid,
}

impl ManifestState {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Missing => "missing",
            Self::Current => "current",
            Self::Stale => "stale",
            Self::Invalid => "invalid",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorldCachePlan {
    pub status: WorldCacheStatus,
    pub thumbnail_count: usize,
    pub thumbnail_ready: usize,
    pub thumbnail_missing: usize,
    pub world_tile_count: usize,
    pub world_tile_ready: usize,
    pub world_tile_missing: usize,
    pub world_lods: Vec<WorldLodPlan>,
    pub dry_run: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorldLodPlan {
    pub lod: usize,
    pub tile_count: usize,
    pub tile_ready: usize,
    pub tile_missing: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorldCellCacheEntry {
    pub source_path: String,
    pub cache_key: String,
    pub image_path: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorldTileCacheEntry {
    pub lod: usize,
    pub tile_column: usize,
    pub tile_row: usize,
    pub start_column: usize,
    pub end_column: usize,
    pub start_row: usize,
    pub end_row: usize,
    pub image_path: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorldCacheSnapshot {
    pub library: Vec<String>,
    pub columns: usize,
    pub rows: usize,
    pub fingerprint: u64,
}

impl WorldCacheSnapshot {
    #[must_use]
    pub fn library_count(&self) -> usize {
        self.library.len()
    }
}

#[must_use]
pub fn auto_world_columns(entry_count: usize) -> usize {
    if entry_count <= 1 {
        return entry_count.max(1);
    }

    let mut columns = 1_usize;
    while columns.saturating_mul(columns) < entry_count {
        columns = columns.saturating_add(1);
    }
    columns
}

#[must_use]
pub fn world_cache_snapshot_for_library(library: Vec<String>) -> WorldCacheSnapshot {
    let fingerprint = fingerprint_paths(&library);
    let columns = auto_world_columns(library.len());
    let rows = library.len().div_ceil(columns);
    WorldCacheSnapshot {
        library,
        columns,
        rows,
        fingerprint,
    }
}

pub fn world_cache_status(config: &MuralConfig) -> Result<WorldCacheStatus, String> {
    world_cache_status_with_edge(config, DEFAULT_WORLD_CELL_THUMBNAIL_EDGE)
}

pub fn world_cache_status_with_edge(
    config: &MuralConfig,
    thumbnail_edge: u32,
) -> Result<WorldCacheStatus, String> {
    let paths = configured_wallpaper_paths(config)?;
    let library = scan_top_level_library(&paths)?;
    let fingerprint = fingerprint_paths(&library);
    let columns = auto_world_columns(library.len());
    let rows = library.len().div_ceil(columns);
    let cache_dir = paths.state_dir.join("cache/world-v1");
    let manifest_path = cache_dir.join("manifest");
    let expected = WorldManifest {
        version: WORLD_CACHE_VERSION,
        order_policy: WORLD_ORDER_POLICY.to_owned(),
        library_count: library.len(),
        columns,
        rows,
        fingerprint,
    };
    let manifest_state = manifest_state(&manifest_path, &expected)?;
    let entries = cell_cache_entries(&library, &cache_dir, thumbnail_edge)?;
    let cell_ready = entries
        .iter()
        .filter(|entry| entry.image_path.is_file())
        .count();
    let cell_missing = entries.len().saturating_sub(cell_ready);
    let tile_entries = tile_pyramid_cache_entries(
        &cache_dir,
        TileCacheSpec {
            thumbnail_edge,
            tile_cells: DEFAULT_WORLD_TILE_CELLS,
            columns,
            rows,
            fingerprint,
        },
        &entries,
    );
    let world_lods = world_lod_statuses(&tile_entries);
    let world_tile_ready = tile_entries
        .iter()
        .filter(|entry| entry.image_path.is_file())
        .count();
    let world_tile_missing = tile_entries.len().saturating_sub(world_tile_ready);
    let ready = manifest_state == ManifestState::Current
        && cell_missing == 0
        && world_tile_missing == 0
        && !library.is_empty();
    let message = match manifest_state {
        ManifestState::Missing => {
            "world cache manifest is missing; run `muralctl world cache index`"
        }
        ManifestState::Current if ready => "world cache is ready for bounded world routes",
        ManifestState::Current => "world cache index is current but image coverage is incomplete; run `muralctl world cache compute --scope all --background --progress`",
        ManifestState::Stale => "world cache manifest is stale; run `muralctl world cache index`",
        ManifestState::Invalid => {
            "world cache manifest is invalid; run `muralctl world cache index`"
        }
    }
    .to_owned();

    Ok(WorldCacheStatus {
        wall_dir: paths.wall_dir,
        state_dir: paths.state_dir,
        cache_dir,
        manifest_path,
        library_count: library.len(),
        columns,
        rows,
        fingerprint,
        order_policy: WORLD_ORDER_POLICY.to_owned(),
        thumbnail_edge,
        cell_ready,
        cell_missing,
        world_tile_ready,
        world_tile_missing,
        world_lods,
        manifest_state,
        ready,
        message,
    })
}

pub fn write_world_cache_index(config: &MuralConfig) -> Result<WorldCacheStatus, String> {
    let paths = configured_wallpaper_paths(config)?;
    let library = scan_top_level_library(&paths)?;
    let snapshot = world_cache_snapshot_for_library(library);
    let cache_dir = paths.state_dir.join("cache/world-v1");
    fs::create_dir_all(&cache_dir).map_err(|error| {
        format!(
            "failed to create world cache directory {}: {error}",
            cache_dir.display()
        )
    })?;
    write_library_snapshot(
        &library_snapshot_path(&cache_dir, snapshot.fingerprint),
        &snapshot.library,
    )?;
    let manifest_path = cache_dir.join("manifest");
    let manifest = WorldManifest {
        version: WORLD_CACHE_VERSION,
        order_policy: WORLD_ORDER_POLICY.to_owned(),
        library_count: snapshot.library_count(),
        columns: snapshot.columns,
        rows: snapshot.rows,
        fingerprint: snapshot.fingerprint,
    };
    write_manifest(&manifest_path, &manifest)?;
    world_cache_status(config)
}

pub fn read_indexed_world_cache_snapshot(
    config: &MuralConfig,
) -> Result<Option<WorldCacheSnapshot>, String> {
    let paths = configured_wallpaper_paths(config)?;
    let cache_dir = paths.state_dir.join("cache/world-v1");
    let manifest_path = cache_dir.join("manifest");
    let content = match fs::read_to_string(&manifest_path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(format!(
                "failed to read world cache manifest {}: {error}",
                manifest_path.display()
            ));
        }
    };
    let Some(manifest) = parse_manifest(&content) else {
        return Ok(None);
    };
    read_world_cache_snapshot_by_manifest(&cache_dir, &manifest)
}

pub fn read_world_cache_snapshot_by_fingerprint(
    config: &MuralConfig,
    fingerprint: u64,
) -> Result<Option<WorldCacheSnapshot>, String> {
    let paths = configured_wallpaper_paths(config)?;
    let cache_dir = paths.state_dir.join("cache/world-v1");
    if let Some(snapshot) = read_library_snapshot(&library_snapshot_path(&cache_dir, fingerprint))?
    {
        if snapshot.fingerprint == fingerprint {
            return Ok(Some(snapshot));
        }
        return Err(format!(
            "world cache snapshot {:016x} has mismatched fingerprint {:016x}",
            fingerprint, snapshot.fingerprint
        ));
    }

    let current_library = scan_top_level_library(&paths)?;
    let current = world_cache_snapshot_for_library(current_library);
    if current.fingerprint == fingerprint {
        return Ok(Some(current));
    }

    Ok(None)
}

pub fn plan_world_cache_compute(
    config: &MuralConfig,
    dry_run: bool,
) -> Result<WorldCachePlan, String> {
    let status = world_cache_status_with_edge(config, DEFAULT_WORLD_CELL_THUMBNAIL_EDGE)?;
    let world_tile_count = status
        .world_lods
        .iter()
        .map(|lod| lod.tile_ready + lod.tile_missing)
        .sum();
    Ok(WorldCachePlan {
        thumbnail_count: status.library_count,
        thumbnail_ready: status.cell_ready,
        thumbnail_missing: status.cell_missing,
        world_tile_count,
        world_tile_ready: status.world_tile_ready,
        world_tile_missing: status.world_tile_missing,
        world_lods: status
            .world_lods
            .iter()
            .map(|lod| WorldLodPlan {
                lod: lod.lod,
                tile_count: lod.tile_ready + lod.tile_missing,
                tile_ready: lod.tile_ready,
                tile_missing: lod.tile_missing,
            })
            .collect(),
        status,
        dry_run,
    })
}

#[must_use]
pub fn world_cache_has_existing_tile_cache(status: &WorldCacheStatus) -> bool {
    status.world_tile_ready > 0 || cache_dir_contains_world_tile(&status.cache_dir)
}

pub fn world_cell_cache_entries(
    config: &MuralConfig,
    thumbnail_edge: u32,
) -> Result<Vec<WorldCellCacheEntry>, String> {
    let paths = configured_wallpaper_paths(config)?;
    let library = scan_top_level_library(&paths)?;
    let cache_dir = paths.state_dir.join("cache/world-v1");
    cell_cache_entries(&library, &cache_dir, thumbnail_edge)
}

pub fn world_cell_cache_entries_for_snapshot(
    config: &MuralConfig,
    snapshot: &WorldCacheSnapshot,
    thumbnail_edge: u32,
) -> Result<Vec<WorldCellCacheEntry>, String> {
    let paths = configured_wallpaper_paths(config)?;
    let cache_dir = paths.state_dir.join("cache/world-v1");
    cell_cache_entries(&snapshot.library, &cache_dir, thumbnail_edge)
}

pub fn world_tile_cache_entries(
    config: &MuralConfig,
    thumbnail_edge: u32,
    tile_cells: usize,
) -> Result<Vec<WorldTileCacheEntry>, String> {
    let status = world_cache_status_with_edge(config, thumbnail_edge)?;
    Ok(tile_cache_entries(
        &status.cache_dir,
        TileCacheSpec {
            thumbnail_edge,
            tile_cells,
            columns: status.columns,
            rows: status.rows,
            fingerprint: status.fingerprint,
        },
        &world_cell_cache_entries(config, thumbnail_edge)?,
    ))
}

pub fn world_tile_pyramid_cache_entries(
    config: &MuralConfig,
    thumbnail_edge: u32,
    tile_cells: usize,
) -> Result<Vec<WorldTileCacheEntry>, String> {
    let status = world_cache_status_with_edge(config, thumbnail_edge)?;
    Ok(tile_pyramid_cache_entries(
        &status.cache_dir,
        TileCacheSpec {
            thumbnail_edge,
            tile_cells,
            columns: status.columns,
            rows: status.rows,
            fingerprint: status.fingerprint,
        },
        &world_cell_cache_entries(config, thumbnail_edge)?,
    ))
}

pub fn world_tile_pyramid_cache_entries_for_snapshot(
    config: &MuralConfig,
    snapshot: &WorldCacheSnapshot,
    thumbnail_edge: u32,
    tile_cells: usize,
) -> Result<Vec<WorldTileCacheEntry>, String> {
    let paths = configured_wallpaper_paths(config)?;
    let cache_dir = paths.state_dir.join("cache/world-v1");
    Ok(tile_pyramid_cache_entries(
        &cache_dir,
        TileCacheSpec {
            thumbnail_edge,
            tile_cells,
            columns: snapshot.columns,
            rows: snapshot.rows,
            fingerprint: snapshot.fingerprint,
        },
        &world_cell_cache_entries_for_snapshot(config, snapshot, thumbnail_edge)?,
    ))
}

pub fn world_tile_pyramid_cache_entries_for_fingerprint(
    config: &MuralConfig,
    fingerprint: u64,
    thumbnail_edge: u32,
    tile_cells: usize,
) -> Result<Vec<WorldTileCacheEntry>, String> {
    let snapshot =
        read_world_cache_snapshot_by_fingerprint(config, fingerprint)?.ok_or_else(|| {
            format!(
                "world cache snapshot {fingerprint:016x} is missing; run `muralctl world cache index`"
            )
        })?;
    world_tile_pyramid_cache_entries_for_snapshot(config, &snapshot, thumbnail_edge, tile_cells)
}

fn cell_cache_entries(
    library: &[String],
    cache_dir: &Path,
    thumbnail_edge: u32,
) -> Result<Vec<WorldCellCacheEntry>, String> {
    let cell_dir = cache_dir.join(format!("cells-{thumbnail_edge}"));
    library
        .iter()
        .map(|source_path| {
            let metadata = fs::metadata(source_path)
                .map_err(|error| format!("failed to stat wallpaper {source_path}: {error}"))?;
            let cache_key = cell_cache_key(
                source_path,
                metadata.len(),
                metadata_mtime_ns(&metadata),
                thumbnail_edge,
            );
            let image_path = cell_dir.join(format!("{cache_key}.png"));
            Ok(WorldCellCacheEntry {
                source_path: source_path.clone(),
                cache_key,
                image_path,
            })
        })
        .collect()
}

#[derive(Clone, Copy)]
struct TileCacheSpec {
    thumbnail_edge: u32,
    tile_cells: usize,
    columns: usize,
    rows: usize,
    fingerprint: u64,
}

fn tile_cache_entries(
    cache_dir: &Path,
    spec: TileCacheSpec,
    cells: &[WorldCellCacheEntry],
) -> Vec<WorldTileCacheEntry> {
    tile_cache_entries_for_lod(cache_dir, spec, 0, cells)
}

fn tile_pyramid_cache_entries(
    cache_dir: &Path,
    spec: TileCacheSpec,
    cells: &[WorldCellCacheEntry],
) -> Vec<WorldTileCacheEntry> {
    let mut entries = Vec::new();
    let mut lod = 0_usize;
    loop {
        let lod_entries = tile_cache_entries_for_lod(cache_dir, spec, lod, cells);
        let done = lod_entries.len() <= 1;
        entries.extend(lod_entries);
        if done {
            break;
        }
        lod = lod.saturating_add(1);
    }
    entries
}

fn tile_cache_entries_for_lod(
    cache_dir: &Path,
    spec: TileCacheSpec,
    lod: usize,
    cells: &[WorldCellCacheEntry],
) -> Vec<WorldTileCacheEntry> {
    let base_tile_cells = spec.tile_cells.max(1);
    let tile_cells = world_lod_tile_cells(base_tile_cells, lod);
    let tile_dir = cache_dir.join(format!(
        "tiles-{}-c{base_tile_cells}-{}x{}-{:016x}/l{lod}",
        spec.thumbnail_edge, spec.columns, spec.rows, spec.fingerprint
    ));
    let tile_columns = spec.columns.div_ceil(tile_cells);
    let tile_rows = spec.rows.div_ceil(tile_cells);
    let mut entries = Vec::with_capacity(tile_columns.saturating_mul(tile_rows));
    for tile_row in 0..tile_rows {
        for tile_column in 0..tile_columns {
            let start_column = tile_column.saturating_mul(tile_cells);
            let start_row = tile_row.saturating_mul(tile_cells);
            let end_column = start_column.saturating_add(tile_cells).min(spec.columns);
            let end_row = start_row.saturating_add(tile_cells).min(spec.rows);
            let content_key = tile_content_key(
                cells,
                spec.columns,
                start_column,
                end_column,
                start_row,
                end_row,
            );
            entries.push(WorldTileCacheEntry {
                lod,
                tile_column,
                tile_row,
                start_column,
                end_column,
                start_row,
                end_row,
                image_path: tile_dir
                    .join(format!("{tile_row:06}-{tile_column:06}-{content_key}.png")),
            });
        }
    }
    entries
}

fn tile_content_key(
    cells: &[WorldCellCacheEntry],
    columns: usize,
    start_column: usize,
    end_column: usize,
    start_row: usize,
    end_row: usize,
) -> String {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for row in start_row..end_row {
        for column in start_column..end_column {
            let index = row.saturating_mul(columns).saturating_add(column);
            if let Some(cell) = cells.get(index) {
                hash_bytes(&mut hash, cell.cache_key.as_bytes());
            }
        }
    }
    format!("{hash:016x}")
}

#[must_use]
pub fn world_lod_tile_cells(tile_cells: usize, lod: usize) -> usize {
    let mut cells = tile_cells.max(1);
    for _ in 0..lod {
        cells = cells.saturating_mul(WORLD_TILE_LOD_BRANCHING);
    }
    cells
}

fn world_lod_statuses(entries: &[WorldTileCacheEntry]) -> Vec<WorldLodCacheStatus> {
    let max_lod = entries.iter().map(|entry| entry.lod).max();
    let Some(max_lod) = max_lod else {
        return Vec::new();
    };

    (0..=max_lod)
        .map(|lod| {
            let tile_ready = entries
                .iter()
                .filter(|entry| entry.lod == lod && entry.image_path.is_file())
                .count();
            let tile_count = entries.iter().filter(|entry| entry.lod == lod).count();
            WorldLodCacheStatus {
                lod,
                tile_ready,
                tile_missing: tile_count.saturating_sub(tile_ready),
            }
        })
        .collect()
}

#[must_use]
pub fn world_lod_cache_statuses(entries: &[WorldTileCacheEntry]) -> Vec<WorldLodCacheStatus> {
    world_lod_statuses(entries)
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct WorldManifest {
    version: u32,
    order_policy: String,
    library_count: usize,
    columns: usize,
    rows: usize,
    fingerprint: u64,
}

fn manifest_state(
    path: &std::path::Path,
    expected: &WorldManifest,
) -> Result<ManifestState, String> {
    let content = match fs::read_to_string(path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ManifestState::Missing);
        }
        Err(error) => {
            return Err(format!(
                "failed to read world cache manifest {}: {error}",
                path.display()
            ));
        }
    };

    let Some(actual) = parse_manifest(&content) else {
        return Ok(ManifestState::Invalid);
    };
    if actual == *expected {
        Ok(ManifestState::Current)
    } else {
        Ok(ManifestState::Stale)
    }
}

fn parse_manifest(content: &str) -> Option<WorldManifest> {
    let mut version = None;
    let mut order_policy = None;
    let mut library_count = None;
    let mut columns = None;
    let mut rows = None;
    let mut fingerprint = None;

    for line in content.lines() {
        let (key, value) = line.split_once('=')?;
        match key {
            "version" => version = value.parse().ok(),
            "order_policy" => order_policy = Some(value.to_owned()),
            "library_count" => library_count = value.parse().ok(),
            "columns" => columns = value.parse().ok(),
            "rows" => rows = value.parse().ok(),
            "fingerprint" => fingerprint = u64::from_str_radix(value, 16).ok(),
            _ => {}
        }
    }

    Some(WorldManifest {
        version: version?,
        order_policy: order_policy.unwrap_or_else(|| WORLD_ORDER_POLICY.to_owned()),
        library_count: library_count?,
        columns: columns?,
        rows: rows?,
        fingerprint: fingerprint?,
    })
}

fn write_manifest(path: &std::path::Path, manifest: &WorldManifest) -> Result<(), String> {
    let tmp = path.with_extension("tmp");
    let mut file = fs::File::create(&tmp)
        .map_err(|error| format!("failed to create {}: {error}", tmp.display()))?;
    writeln!(file, "version={}", manifest.version)
        .map_err(|error| format!("failed to write {}: {error}", tmp.display()))?;
    writeln!(file, "order_policy={}", manifest.order_policy)
        .map_err(|error| format!("failed to write {}: {error}", tmp.display()))?;
    writeln!(file, "library_count={}", manifest.library_count)
        .map_err(|error| format!("failed to write {}: {error}", tmp.display()))?;
    writeln!(file, "columns={}", manifest.columns)
        .map_err(|error| format!("failed to write {}: {error}", tmp.display()))?;
    writeln!(file, "rows={}", manifest.rows)
        .map_err(|error| format!("failed to write {}: {error}", tmp.display()))?;
    writeln!(file, "fingerprint={:016x}", manifest.fingerprint)
        .map_err(|error| format!("failed to write {}: {error}", tmp.display()))?;
    file.sync_all()
        .map_err(|error| format!("failed to sync {}: {error}", tmp.display()))?;
    fs::rename(&tmp, path).map_err(|error| {
        format!(
            "failed to replace world cache manifest {}: {error}",
            path.display()
        )
    })
}

fn read_world_cache_snapshot_by_manifest(
    cache_dir: &Path,
    manifest: &WorldManifest,
) -> Result<Option<WorldCacheSnapshot>, String> {
    let Some(snapshot) =
        read_library_snapshot(&library_snapshot_path(cache_dir, manifest.fingerprint))?
    else {
        return Ok(None);
    };
    if snapshot.library_count() != manifest.library_count
        || snapshot.columns != manifest.columns
        || snapshot.rows != manifest.rows
        || snapshot.fingerprint != manifest.fingerprint
    {
        return Err(format!(
            "world cache snapshot {:016x} does not match the indexed manifest",
            manifest.fingerprint
        ));
    }
    Ok(Some(snapshot))
}

fn library_snapshot_path(cache_dir: &Path, fingerprint: u64) -> PathBuf {
    cache_dir.join(format!("library-{fingerprint:016x}.paths"))
}

fn write_library_snapshot(path: &Path, library: &[String]) -> Result<(), String> {
    let tmp = path.with_extension("tmp");
    let mut file = fs::File::create(&tmp)
        .map_err(|error| format!("failed to create {}: {error}", tmp.display()))?;
    for entry in library {
        writeln!(file, "{entry}")
            .map_err(|error| format!("failed to write {}: {error}", tmp.display()))?;
    }
    file.sync_all()
        .map_err(|error| format!("failed to sync {}: {error}", tmp.display()))?;
    fs::rename(&tmp, path).map_err(|error| {
        format!(
            "failed to replace world cache snapshot {}: {error}",
            path.display()
        )
    })
}

fn read_library_snapshot(path: &Path) -> Result<Option<WorldCacheSnapshot>, String> {
    let content = match fs::read_to_string(path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(format!(
                "failed to read world cache snapshot {}: {error}",
                path.display()
            ));
        }
    };
    Ok(Some(world_cache_snapshot_for_library(
        content.lines().map(ToOwned::to_owned).collect(),
    )))
}

fn cache_dir_contains_world_tile(cache_dir: &Path) -> bool {
    let Ok(entries) = fs::read_dir(cache_dir) else {
        return false;
    };
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_dir() {
            continue;
        }
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !name.starts_with("tiles-") {
            continue;
        }
        if tile_tree_contains_png(&entry.path()) {
            return true;
        }
    }
    false
}

fn tile_tree_contains_png(path: &Path) -> bool {
    let Ok(entries) = fs::read_dir(path) else {
        return false;
    };
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let path = entry.path();
        if file_type.is_file() {
            if path.extension().is_some_and(|extension| extension == "png") {
                return true;
            }
        } else if file_type.is_dir() && tile_tree_contains_png(&path) {
            return true;
        }
    }
    false
}

fn fingerprint_paths(paths: &[String]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for path in paths {
        for byte in path.as_bytes().iter().copied().chain([0xff]) {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    hash
}

fn cell_cache_key(path: &str, len: u64, mtime_ns: u128, thumbnail_edge: u32) -> String {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    hash_bytes(&mut hash, path.as_bytes());
    hash_bytes(&mut hash, &len.to_le_bytes());
    hash_bytes(&mut hash, &mtime_ns.to_le_bytes());
    hash_bytes(&mut hash, &thumbnail_edge.to_le_bytes());
    hash_bytes(&mut hash, &WORLD_CELL_CACHE_ALGORITHM.to_le_bytes());
    format!("{hash:016x}")
}

fn hash_bytes(hash: &mut u64, bytes: &[u8]) {
    for byte in bytes.iter().copied().chain([0xff]) {
        *hash ^= u64::from(byte);
        *hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
}

fn metadata_mtime_ns(metadata: &fs::Metadata) -> u128 {
    metadata
        .modified()
        .ok()
        .and_then(|mtime| mtime.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |duration| duration.as_nanos())
}

#[cfg(test)]
mod tests {
    use std::env;
    use std::fs;

    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = env::temp_dir().join(format!(
            "mural-world-cache-test-{}-{name}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn test_cell_entries(count: usize) -> Vec<WorldCellCacheEntry> {
        (0..count)
            .map(|index| WorldCellCacheEntry {
                source_path: format!("/walls/{index}.jpg"),
                cache_key: format!("cell-key-{index}"),
                image_path: PathBuf::from(format!("/cache/cells/{index}.png")),
            })
            .collect()
    }

    fn test_config(wall_dir: PathBuf, state_dir: PathBuf, quarantine_dir: PathBuf) -> MuralConfig {
        let transitions = crate::actions::ActionTransitions {
            next: mural_ipc::Transition::Cut,
            back: mural_ipc::Transition::Cut,
            shift_forward: mural_ipc::Transition::Cut,
            shift_back: mural_ipc::Transition::Cut,
            replace: mural_ipc::Transition::Cut,
            quarantine: mural_ipc::Transition::Cut,
            startup: mural_ipc::Transition::Cut,
        };
        MuralConfig {
            wall_dir: Some(wall_dir),
            state_dir: Some(state_dir),
            quarantine_dir: Some(quarantine_dir),
            favorite_weight: None,
            max_history: None,
            scale_mode: mural_ipc::ScaleMode::Fill,
            decode_full_workers: 1,
            canvas_thumbnail_max_edge: 1,
            canvas_cache_workers: 1,
            canvas_cache_backend: mural_ipc::CacheBackend::Internal,
            canvas_cache_memory_bytes: 1,
            actions: crate::actions::ActionMap::new(&transitions),
            world_fallback_transition: None,
        }
    }

    #[test]
    fn auto_columns_make_nearly_square_grid() {
        assert_eq!(auto_world_columns(0), 1);
        assert_eq!(auto_world_columns(1), 1);
        assert_eq!(auto_world_columns(10), 4);
        assert_eq!(auto_world_columns(100), 10);
    }

    #[test]
    fn tile_pyramid_entries_run_to_single_overview_tile() {
        let cells = test_cell_entries(253 * 253);
        let entries = tile_pyramid_cache_entries(
            Path::new("/cache"),
            TileCacheSpec {
                thumbnail_edge: 384,
                tile_cells: 8,
                columns: 253,
                rows: 253,
                fingerprint: 0x1234_5678_9abc_def0,
            },
            &cells,
        );
        let statuses = world_lod_statuses(&entries);

        assert_eq!(
            statuses
                .iter()
                .map(|status| (status.lod, status.tile_missing))
                .collect::<Vec<_>>(),
            vec![(0, 1024), (1, 16), (2, 1)]
        );
        assert_eq!(world_lod_tile_cells(8, 0), 8);
        assert_eq!(world_lod_tile_cells(8, 1), 64);
        assert_eq!(world_lod_tile_cells(8, 2), 512);
        assert!(
            entries
                .iter()
                .any(|entry| entry.lod == 2 && entry.start_column == 0 && entry.end_column == 253)
        );
    }

    #[test]
    fn world_tile_paths_include_layout_and_fingerprint() {
        let cells = test_cell_entries(10 * 7);
        let entries = tile_cache_entries(
            Path::new("/cache"),
            TileCacheSpec {
                thumbnail_edge: 384,
                tile_cells: 8,
                columns: 10,
                rows: 7,
                fingerprint: 0x0123_4567_89ab_cdef,
            },
            &cells,
        );

        assert!(
            entries[0]
                .image_path
                .to_string_lossy()
                .contains("tiles-384-c8-10x7-0123456789abcdef/l0")
        );
    }

    #[test]
    fn world_tile_paths_change_when_covered_cell_key_changes() {
        let mut cells = test_cell_entries(64);
        let spec = TileCacheSpec {
            thumbnail_edge: 384,
            tile_cells: 8,
            columns: 8,
            rows: 8,
            fingerprint: 0x0123,
        };
        let before = tile_cache_entries(Path::new("/cache"), spec, &cells)[0]
            .image_path
            .clone();
        cells[0].cache_key = "changed-cell-key".to_owned();
        let after = tile_cache_entries(Path::new("/cache"), spec, &cells)[0]
            .image_path
            .clone();

        assert_ne!(before, after);
    }

    #[test]
    fn manifest_parse_defaults_missing_order_policy_to_v1() {
        let manifest = parse_manifest(
            "version=1\nlibrary_count=2\ncolumns=2\nrows=1\nfingerprint=0000000000001234\n",
        )
        .unwrap();

        assert_eq!(manifest.order_policy, WORLD_ORDER_POLICY);
    }

    #[test]
    fn manifest_state_rejects_different_order_policy() {
        let root = temp_dir("manifest-policy");
        let path = root.join("manifest");
        fs::write(
            &path,
            "version=1\norder_policy=other\nlibrary_count=2\ncolumns=2\nrows=1\nfingerprint=0000000000001234\n",
        )
        .unwrap();
        let expected = WorldManifest {
            version: WORLD_CACHE_VERSION,
            order_policy: WORLD_ORDER_POLICY.to_owned(),
            library_count: 2,
            columns: 2,
            rows: 1,
            fingerprint: 0x1234,
        };

        assert_eq!(
            manifest_state(&path, &expected).unwrap(),
            ManifestState::Stale
        );
    }

    #[test]
    fn status_tracks_missing_and_current_manifest() {
        let root = temp_dir("status");
        let wall_dir = root.join("walls");
        let state_dir = root.join("state");
        let quarantine_dir = wall_dir.join(".quarantine");
        fs::create_dir_all(&wall_dir).unwrap();
        fs::write(wall_dir.join("b.png"), b"b").unwrap();
        fs::write(wall_dir.join("a.jpg"), b"a").unwrap();
        fs::write(wall_dir.join("ignored.txt"), b"x").unwrap();

        let config = test_config(wall_dir, state_dir, quarantine_dir);

        let status = world_cache_status(&config).unwrap();
        assert_eq!(status.library_count, 2);
        assert_eq!(status.manifest_state, ManifestState::Missing);

        let status = write_world_cache_index(&config).unwrap();
        assert_eq!(status.manifest_state, ManifestState::Current);
        assert_eq!(status.order_policy, WORLD_ORDER_POLICY);
        assert!(
            fs::read_to_string(&status.manifest_path)
                .unwrap()
                .contains("order_policy=path-snapshot-v1")
        );
        assert!(!status.ready);
    }

    #[test]
    fn indexed_snapshot_survives_later_library_changes() {
        let root = temp_dir("snapshot-stale");
        let wall_dir = root.join("walls");
        let state_dir = root.join("state");
        let quarantine_dir = wall_dir.join(".quarantine");
        fs::create_dir_all(&wall_dir).unwrap();
        let removed = wall_dir.join("b.png");
        fs::write(wall_dir.join("a.jpg"), b"a").unwrap();
        fs::write(&removed, b"b").unwrap();
        let config = test_config(wall_dir, state_dir, quarantine_dir);

        let indexed = write_world_cache_index(&config).unwrap();
        fs::remove_file(&removed).unwrap();
        let status = world_cache_status(&config).unwrap();
        let snapshot = read_indexed_world_cache_snapshot(&config).unwrap().unwrap();

        assert_eq!(status.manifest_state, ManifestState::Stale);
        assert_eq!(snapshot.fingerprint, indexed.fingerprint);
        assert_eq!(snapshot.library_count(), 2);
        assert!(snapshot.library.iter().any(|path| path.ends_with("b.png")));
    }

    #[test]
    fn cell_cache_entries_report_deleted_source_paths() {
        let root = temp_dir("deleted-source");
        let missing = root.join("missing.jpg");
        let error = cell_cache_entries(
            &[missing.to_string_lossy().into_owned()],
            &root.join("cache"),
            DEFAULT_WORLD_CELL_THUMBNAIL_EDGE,
        )
        .unwrap_err();

        assert!(error.contains("failed to stat wallpaper"));
        assert!(error.contains("missing.jpg"));
    }

    #[test]
    fn status_marks_manifest_stale_after_source_delete() {
        let root = temp_dir("deleted-status");
        let wall_dir = root.join("walls");
        let state_dir = root.join("state");
        let quarantine_dir = wall_dir.join(".quarantine");
        fs::create_dir_all(&wall_dir).unwrap();
        let removed = wall_dir.join("b.png");
        fs::write(wall_dir.join("a.jpg"), b"a").unwrap();
        fs::write(&removed, b"b").unwrap();
        let config = test_config(wall_dir, state_dir, quarantine_dir);

        assert_eq!(
            write_world_cache_index(&config).unwrap().manifest_state,
            ManifestState::Current
        );
        fs::remove_file(&removed).unwrap();
        let status = world_cache_status(&config).unwrap();

        assert_eq!(status.library_count, 1);
        assert_eq!(status.manifest_state, ManifestState::Stale);
        assert!(!status.ready);
    }

    #[test]
    fn status_marks_manifest_stale_after_source_quarantine() {
        let root = temp_dir("quarantined-status");
        let wall_dir = root.join("walls");
        let state_dir = root.join("state");
        let quarantine_dir = wall_dir.join(".quarantine");
        fs::create_dir_all(&wall_dir).unwrap();
        fs::create_dir_all(&quarantine_dir).unwrap();
        let source = wall_dir.join("b.png");
        let quarantined = quarantine_dir.join("b.png");
        fs::write(wall_dir.join("a.jpg"), b"a").unwrap();
        fs::write(&source, b"b").unwrap();
        let config = test_config(wall_dir, state_dir, quarantine_dir);

        assert_eq!(
            write_world_cache_index(&config).unwrap().manifest_state,
            ManifestState::Current
        );
        fs::rename(&source, &quarantined).unwrap();
        let status = world_cache_status(&config).unwrap();
        let cell_entries =
            world_cell_cache_entries(&config, DEFAULT_WORLD_CELL_THUMBNAIL_EDGE).unwrap();

        assert_eq!(status.library_count, 1);
        assert_eq!(status.manifest_state, ManifestState::Stale);
        assert!(!status.ready);
        assert!(
            cell_entries
                .iter()
                .all(|entry| !entry.source_path.contains(".quarantine"))
        );
    }
}
