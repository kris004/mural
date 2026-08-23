use std::sync::{Arc, Mutex, mpsc};
use std::thread;

use calloop::channel as calloop_channel;

#[derive(Clone)]
pub(crate) struct DecodedImage {
    pub(crate) width: i32,
    pub(crate) height: i32,
    pub(crate) pixels: Vec<u8>,
}

impl DecodedImage {
    pub(crate) fn load(path: &str) -> Result<Self, String> {
        let image = image::ImageReader::open(path)
            .map_err(|error| format!("failed to open image {path}: {error}"))?
            .with_guessed_format()
            .map_err(|error| format!("failed to detect image format for {path}: {error}"))?
            .decode()
            .map_err(|error| format!("failed to decode image {path}: {error}"))?
            .into_rgba8();

        let width = i32::try_from(image.width())
            .map_err(|_| format!("image width is too large for GL: {path}"))?;
        let height = i32::try_from(image.height())
            .map_err(|_| format!("image height is too large for GL: {path}"))?;
        if width <= 0 || height <= 0 {
            return Err(format!("image has invalid dimensions: {path}"));
        }

        Ok(Self {
            width,
            height,
            pixels: image.into_raw(),
        })
    }

    pub(crate) fn byte_len(&self) -> usize {
        self.pixels.len()
    }
}

pub(crate) struct DecodeJob {
    pub(crate) id: u64,
    pub(crate) image_path: String,
}

pub(crate) struct DecodeResult {
    pub(crate) id: u64,
    pub(crate) result: Result<DecodedImage, String>,
}

pub(crate) type DecodeWorkerChannels = (
    mpsc::Sender<DecodeJob>,
    calloop_channel::Channel<DecodeResult>,
);

pub(crate) fn spawn_decode_workers(full_workers: usize) -> Result<DecodeWorkerChannels, String> {
    let (full_tx, full_rx) = mpsc::channel::<DecodeJob>();
    let (result_tx, result_rx) = calloop_channel::channel::<DecodeResult>();
    spawn_decode_pool("mural-decode", full_workers, full_rx, &result_tx)?;

    Ok((full_tx, result_rx))
}

fn spawn_decode_pool(
    name: &str,
    workers: usize,
    job_rx: mpsc::Receiver<DecodeJob>,
    result_tx: &calloop_channel::Sender<DecodeResult>,
) -> Result<(), String> {
    let job_rx = Arc::new(Mutex::new(job_rx));
    for index in 0..workers.max(1) {
        let job_rx = Arc::clone(&job_rx);
        let result_tx = result_tx.clone();
        let worker_name = format!("{name}-{index}");
        thread::Builder::new()
            .name(worker_name.clone())
            .spawn(move || decode_worker(&job_rx, &result_tx))
            .map_err(|error| format!("failed to spawn {worker_name}: {error}"))?;
    }

    Ok(())
}

fn decode_worker(
    job_rx: &Mutex<mpsc::Receiver<DecodeJob>>,
    result_tx: &calloop_channel::Sender<DecodeResult>,
) {
    loop {
        let Ok(job) = job_rx
            .lock()
            .expect("decode receiver mutex poisoned")
            .recv()
        else {
            break;
        };
        let result = DecodedImage::load(&job.image_path);
        if result_tx.send(DecodeResult { id: job.id, result }).is_err() {
            break;
        }
    }
}
