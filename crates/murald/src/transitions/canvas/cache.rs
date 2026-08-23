use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::{self, Command, Stdio};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::UNIX_EPOCH;

use calloop::channel as calloop_channel;
use mural_ipc::CacheBackend;

use crate::decode::DecodedImage;

struct ThumbnailCache {
    max_bytes: usize,
    current_bytes: usize,
    clock: u64,
    entries: BTreeMap<String, ThumbnailCacheEntry>,
}

struct ThumbnailCacheEntry {
    image: DecodedImage,
    bytes: usize,
    last_used: u64,
}

impl ThumbnailCache {
    fn new(max_bytes: usize) -> Self {
        Self {
            max_bytes,
            current_bytes: 0,
            clock: 0,
            entries: BTreeMap::new(),
        }
    }

    fn get(&mut self, key: &str) -> Option<DecodedImage> {
        self.clock = self.clock.wrapping_add(1);
        let entry = self.entries.get_mut(key)?;
        entry.last_used = self.clock;
        Some(entry.image.clone())
    }

    fn contains(&self, key: &str) -> bool {
        self.entries.contains_key(key)
    }

    fn insert(&mut self, key: String, image: DecodedImage) {
        self.clock = self.clock.wrapping_add(1);
        let bytes = image.byte_len();
        if let Some(old) = self.entries.remove(&key) {
            self.current_bytes = self.current_bytes.saturating_sub(old.bytes);
        }
        self.current_bytes = self.current_bytes.saturating_add(bytes);
        self.entries.insert(
            key,
            ThumbnailCacheEntry {
                image,
                bytes,
                last_used: self.clock,
            },
        );
        self.evict_to_limit();
    }

    fn clear(&mut self) {
        self.current_bytes = 0;
        self.entries.clear();
    }

    fn evict_to_limit(&mut self) {
        while self.current_bytes > self.max_bytes && !self.entries.is_empty() {
            let Some(path) = self
                .entries
                .iter()
                .min_by_key(|(_path, entry)| entry.last_used)
                .map(|(path, _entry)| path.clone())
            else {
                return;
            };
            if let Some(entry) = self.entries.remove(&path) {
                self.current_bytes = self.current_bytes.saturating_sub(entry.bytes);
            }
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CacheStatus {
    pub(crate) ready: usize,
    pub(crate) pending: usize,
    pub(crate) failed: usize,
}

pub(crate) struct CanvasCache {
    root: PathBuf,
    max_edge: u32,
    default_backend: CacheBackend,
    memory: ThumbnailCache,
    pending: BTreeSet<String>,
    failed: usize,
    result_tx: calloop_channel::Sender<CanvasCacheResult>,
}

#[derive(Clone, Debug)]
struct CanvasCacheJob {
    source_path: String,
    entry: CanvasCacheEntry,
    max_edge: u32,
    backend: CacheBackend,
}

#[derive(Clone, Debug)]
struct CanvasCacheEntry {
    key: String,
    image_path: PathBuf,
    meta_path: PathBuf,
    source_len: u64,
    source_mtime_ns: u128,
}

#[derive(Clone, Debug)]
enum CanvasCacheEntryError {
    Missing(String),
    Other(String),
}

impl std::fmt::Display for CanvasCacheEntryError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Missing(message) | Self::Other(message) => formatter.write_str(message),
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct CanvasCacheResult {
    cache_key: String,
    pub(crate) source_path: String,
    pub(crate) result: Result<(), String>,
}

impl CanvasCache {
    pub(crate) fn new(
        root: PathBuf,
        max_edge: u32,
        default_backend: CacheBackend,
        max_memory_bytes: usize,
        result_tx: calloop_channel::Sender<CanvasCacheResult>,
    ) -> Self {
        Self {
            root,
            max_edge,
            default_backend,
            memory: ThumbnailCache::new(max_memory_bytes),
            pending: BTreeSet::new(),
            failed: 0,
            result_tx,
        }
    }

    pub(crate) fn status(&self) -> CacheStatus {
        let ready = fs::read_dir(&self.root).map_or(0, |entries| {
            entries
                .filter_map(Result::ok)
                .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "jpg"))
                .count()
        });
        CacheStatus {
            ready,
            pending: self.pending.len(),
            failed: self.failed,
        }
    }

    pub(crate) const fn empty_status() -> CacheStatus {
        CacheStatus {
            ready: 0,
            pending: 0,
            failed: 0,
        }
    }

    pub(crate) fn clear(&mut self) -> Result<usize, String> {
        let removed = clear_canvas_cache_root(&self.root)?;
        self.memory.clear();
        self.pending.clear();
        self.failed = 0;
        Ok(removed)
    }

    pub(crate) fn get_image(&mut self, source_path: &str) -> Option<DecodedImage> {
        let backend = Self::effective_backend(self.default_backend);
        let entry = self.entry(source_path, backend).ok()?;
        if let Some(image) = self.memory.get(&entry.key) {
            return Some(image);
        }
        if !entry.image_path.is_file() {
            return None;
        }
        let cache_path = entry.image_path.to_string_lossy();
        match DecodedImage::load(&cache_path) {
            Ok(image) => {
                self.memory.insert(entry.key, image.clone());
                Some(image)
            }
            Err(error) => {
                eprintln!(
                    "murald: failed to load cached canvas thumbnail {}: {error}",
                    entry.image_path.display()
                );
                None
            }
        }
    }

    pub(crate) fn schedule(
        &mut self,
        paths: Vec<String>,
        workers: usize,
        backend: CacheBackend,
    ) -> usize {
        let backend = Self::effective_backend(backend);
        let mut deduped = BTreeSet::new();
        let mut work = Vec::new();

        if let Err(error) = fs::create_dir_all(&self.root) {
            eprintln!(
                "murald: failed to create canvas cache directory {}: {error}",
                self.root.display()
            );
            return 0;
        }

        for path in paths {
            if path.is_empty() || !deduped.insert(path.clone()) {
                continue;
            }
            let entry = match self.entry(&path, backend) {
                Ok(entry) => entry,
                Err(CanvasCacheEntryError::Missing(_)) => {
                    continue;
                }
                Err(error) => {
                    eprintln!("murald: skipping canvas cache path {path}: {error}");
                    continue;
                }
            };
            if entry.image_path.is_file()
                || self.memory.contains(&entry.key)
                || self.pending.contains(&entry.key)
            {
                continue;
            }
            self.pending.insert(entry.key.clone());
            work.push(CanvasCacheJob {
                source_path: path,
                entry,
                max_edge: self.max_edge,
                backend,
            });
        }

        let scheduled = work.len();
        if scheduled == 0 {
            return 0;
        }
        spawn_canvas_cache_workers(work, workers, &self.result_tx);
        scheduled
    }

    pub(crate) fn accept_result(&mut self, result: &CanvasCacheResult) {
        self.pending.remove(&result.cache_key);
        if result.result.is_err() {
            self.failed = self.failed.saturating_add(1);
        }
    }

    fn entry(
        &self,
        source_path: &str,
        backend: CacheBackend,
    ) -> Result<CanvasCacheEntry, CanvasCacheEntryError> {
        let metadata = fs::metadata(source_path).map_err(|error| {
            if error.kind() == ErrorKind::NotFound {
                CanvasCacheEntryError::Missing(format!("source image no longer exists: {error}"))
            } else {
                CanvasCacheEntryError::Other(format!("failed to stat source image: {error}"))
            }
        })?;
        if !metadata.is_file() {
            return Err(CanvasCacheEntryError::Other(
                "source image is not a file".to_owned(),
            ));
        }
        let source_len = metadata.len();
        let source_mtime_ns = metadata_mtime_ns(&metadata);
        let key = canvas_cache_key(
            source_path,
            source_len,
            source_mtime_ns,
            self.max_edge,
            backend,
        );
        Ok(CanvasCacheEntry {
            image_path: self.root.join(format!("{key}.jpg")),
            meta_path: self.root.join(format!("{key}.meta")),
            key,
            source_len,
            source_mtime_ns,
        })
    }

    pub(crate) fn effective_backend(backend: CacheBackend) -> CacheBackend {
        match backend {
            CacheBackend::Auto if command_available("vipsthumbnail") => CacheBackend::Vips,
            CacheBackend::Auto => CacheBackend::Internal,
            other => other,
        }
    }
}

pub(crate) fn clear_canvas_cache_root(root: &Path) -> Result<usize, String> {
    let removed = canvas_cache_file_count(root);
    if root.is_dir() {
        fs::remove_dir_all(root)
            .map_err(|error| format!("failed to clear canvas cache {}: {error}", root.display()))?;
    } else if root.exists() {
        fs::remove_file(root)
            .map_err(|error| format!("failed to clear canvas cache {}: {error}", root.display()))?;
    }
    Ok(removed)
}

fn canvas_cache_file_count(root: &Path) -> usize {
    fs::read_dir(root).map_or(0, |entries| {
        entries
            .filter_map(Result::ok)
            .filter(|entry| entry.path().is_file())
            .count()
    })
}

fn spawn_canvas_cache_workers(
    work: Vec<CanvasCacheJob>,
    workers: usize,
    result_tx: &calloop_channel::Sender<CanvasCacheResult>,
) {
    let worker_count = workers
        .max(1)
        .min(work.len())
        .min(std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get));
    let (job_tx, job_rx) = mpsc::channel::<CanvasCacheJob>();
    let job_rx = Arc::new(Mutex::new(job_rx));
    for job in work {
        if job_tx.send(job).is_err() {
            return;
        }
    }
    drop(job_tx);

    for index in 0..worker_count {
        let job_rx = Arc::clone(&job_rx);
        let result_tx = result_tx.clone();
        let worker_name = format!("mural-canvas-cache-{index}");
        if let Err(error) = thread::Builder::new()
            .name(worker_name.clone())
            .spawn(move || canvas_cache_worker(&job_rx, &result_tx))
        {
            eprintln!("murald: failed to spawn {worker_name}: {error}");
        }
    }
}

fn canvas_cache_worker(
    job_rx: &Arc<Mutex<mpsc::Receiver<CanvasCacheJob>>>,
    result_tx: &calloop_channel::Sender<CanvasCacheResult>,
) {
    loop {
        let Ok(job) = job_rx
            .lock()
            .expect("canvas cache receiver mutex poisoned")
            .recv()
        else {
            break;
        };
        let result = generate_canvas_cache_entry(&job);
        if result_tx
            .send(CanvasCacheResult {
                cache_key: job.entry.key,
                source_path: job.source_path,
                result,
            })
            .is_err()
        {
            break;
        }
    }
}

fn generate_canvas_cache_entry(job: &CanvasCacheJob) -> Result<(), String> {
    let parent = job
        .entry
        .image_path
        .parent()
        .ok_or_else(|| "canvas cache image has no parent directory".to_owned())?;
    fs::create_dir_all(parent).map_err(|error| {
        format!(
            "failed to create canvas cache directory {}: {error}",
            parent.display()
        )
    })?;

    let tmp_image = job.entry.image_path.with_extension(format!(
        "tmp-{}-{}.jpg",
        process::id(),
        thread_name_suffix()
    ));
    let tmp_meta = job.entry.meta_path.with_extension(format!(
        "tmp-{}-{}.meta",
        process::id(),
        thread_name_suffix()
    ));

    let result = match job.backend {
        CacheBackend::Vips => generate_canvas_cache_vips(job, &tmp_image),
        CacheBackend::Internal | CacheBackend::Auto => {
            generate_canvas_cache_internal(job, &tmp_image)
        }
    };
    if let Err(error) = result {
        let _ = fs::remove_file(&tmp_image);
        let _ = fs::remove_file(&tmp_meta);
        return Err(error);
    }

    write_canvas_cache_metadata(job, &tmp_meta)?;
    fs::rename(&tmp_image, &job.entry.image_path).map_err(|error| {
        format!(
            "failed to install canvas thumbnail {}: {error}",
            job.entry.image_path.display()
        )
    })?;
    fs::rename(&tmp_meta, &job.entry.meta_path).map_err(|error| {
        format!(
            "failed to install canvas thumbnail metadata {}: {error}",
            job.entry.meta_path.display()
        )
    })?;
    Ok(())
}

fn generate_canvas_cache_vips(job: &CanvasCacheJob, tmp_image: &Path) -> Result<(), String> {
    let output = format!("{}[Q=92]", tmp_image.display());
    let status = Command::new("vipsthumbnail")
        .arg(&job.source_path)
        .arg("-s")
        .arg(job.max_edge.to_string())
        .arg("-o")
        .arg(output)
        .arg("--vips-concurrency=1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|error| format!("failed to run vipsthumbnail: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("vipsthumbnail exited with {status}"))
    }
}

fn generate_canvas_cache_internal(job: &CanvasCacheJob, tmp_image: &Path) -> Result<(), String> {
    let image = image::ImageReader::open(&job.source_path)
        .map_err(|error| format!("failed to open image: {error}"))?
        .with_guessed_format()
        .map_err(|error| format!("failed to detect image format: {error}"))?
        .decode()
        .map_err(|error| format!("failed to decode image: {error}"))?
        .thumbnail(job.max_edge, job.max_edge)
        .into_rgb8();
    let mut file = fs::File::create(tmp_image)
        .map_err(|error| format!("failed to create {}: {error}", tmp_image.display()))?;
    let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut file, 92);
    encoder
        .encode_image(&image)
        .map_err(|error| format!("failed to encode JPEG thumbnail: {error}"))?;
    Ok(())
}

fn write_canvas_cache_metadata(job: &CanvasCacheJob, tmp_meta: &Path) -> Result<(), String> {
    let metadata = format!(
        "version\t1\nsource_path\t{}\nsource_len\t{}\nsource_mtime_ns\t{}\nmax_edge\t{}\nbackend\t{}\n",
        escape_metadata_value(&job.source_path),
        job.entry.source_len,
        job.entry.source_mtime_ns,
        job.max_edge,
        job.backend.as_str()
    );
    fs::write(tmp_meta, metadata)
        .map_err(|error| format!("failed to write {}: {error}", tmp_meta.display()))
}

fn metadata_mtime_ns(metadata: &fs::Metadata) -> u128 {
    metadata
        .modified()
        .ok()
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |duration| duration.as_nanos())
}

fn canvas_cache_key(
    source_path: &str,
    source_len: u64,
    source_mtime_ns: u128,
    max_edge: u32,
    backend: CacheBackend,
) -> String {
    let mut hash = Fnv1a64::new();
    hash.write(b"mural-canvas-v1\0");
    hash.write(source_path.as_bytes());
    hash.write(&source_len.to_ne_bytes());
    hash.write(&source_mtime_ns.to_ne_bytes());
    hash.write(&max_edge.to_ne_bytes());
    hash.write(backend.as_str().as_bytes());
    format!("{:016x}", hash.finish())
}

struct Fnv1a64(u64);

impl Fnv1a64 {
    const fn new() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }

    fn write(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.0 ^= u64::from(*byte);
            self.0 = self.0.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }

    const fn finish(self) -> u64 {
        self.0
    }
}

fn command_available(command: &str) -> bool {
    let Some(path) = env::var_os("PATH") else {
        return false;
    };
    env::split_paths(&path).any(|dir| dir.join(command).is_file())
}

fn escape_metadata_value(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('\t', "\\t")
        .replace('\n', "\\n")
}

fn thread_name_suffix() -> String {
    thread::current()
        .name()
        .unwrap_or("worker")
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character
            } else {
                '-'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_cache_root(name: &str) -> PathBuf {
        let root =
            env::temp_dir().join(format!("mural-canvas-cache-test-{}-{name}", process::id()));
        let _ = fs::remove_dir_all(&root);
        root
    }

    #[test]
    fn missing_sources_are_skipped_without_failed_cache_entries() {
        let (result_tx, _result_rx) = calloop_channel::channel::<CanvasCacheResult>();
        let root = temp_cache_root("missing-source");
        let missing = root.join("missing.jpg");
        let mut cache = CanvasCache::new(
            root.clone(),
            1536,
            CacheBackend::Internal,
            1024 * 1024,
            result_tx,
        );

        let scheduled = cache.schedule(
            vec![missing.to_string_lossy().into_owned()],
            1,
            CacheBackend::Internal,
        );

        assert_eq!(scheduled, 0);
        assert_eq!(cache.status().pending, 0);
        assert_eq!(cache.status().failed, 0);

        let _ = fs::remove_dir_all(root);
    }
}
