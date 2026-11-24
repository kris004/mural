use std::sync::{Arc, Mutex, mpsc};
use std::thread;

use calloop::channel as calloop_channel;

#[derive(Debug)]
pub(crate) enum ImageLoadError {
    Invalid(mural_ipc::InvalidImage),
    Other(String),
}

impl std::fmt::Display for ImageLoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(image) => f.write_str(&image.message),
            Self::Other(message) => f.write_str(message),
        }
    }
}

impl From<ImageLoadError> for mural_ipc::Response {
    fn from(error: ImageLoadError) -> Self {
        match error {
            ImageLoadError::Invalid(image) => Self::InvalidImage(image),
            ImageLoadError::Other(message) => Self::Error { message },
        }
    }
}

impl From<String> for ImageLoadError {
    fn from(message: String) -> Self {
        Self::Other(message)
    }
}

impl From<ImageLoadError> for String {
    fn from(error: ImageLoadError) -> Self {
        error.to_string()
    }
}

fn image_reader<'a>(
    file: &'a std::fs::File,
    path: &str,
) -> std::io::Result<image::ImageReader<std::io::BufReader<&'a std::fs::File>>> {
    let mut reader = image::ImageReader::new(std::io::BufReader::new(file));
    if let Ok(format) = image::ImageFormat::from_path(path) {
        reader.set_format(format);
    }
    reader.with_guessed_format()
}

fn rejected_source(error: &image::ImageError, mut file: &std::fs::File, path: &str) -> bool {
    use image::{ImageDecoder as _, ImageError, error::LimitErrorKind};
    use std::io::{ErrorKind, Seek as _};
    match error {
        ImageError::Decoding(_) | ImageError::Unsupported(_) => true,
        ImageError::IoError(error) => error.kind() == ErrorKind::UnexpectedEof,
        ImageError::Limits(error) => match error.kind() {
            LimitErrorKind::DimensionError => true,
            // This variant can also mean an actual allocation failure. Only
            // quarantine when the declared output alone exceeds the decoder budget.
            LimitErrorKind::InsufficientMemory => {
                file.rewind().is_ok()
                    && image_reader(file, path)
                        .ok()
                        .and_then(|r| r.into_decoder().ok())
                        .is_some_and(|d| {
                            image::Limits::default()
                                .max_alloc
                                .is_some_and(|max| d.total_bytes() > max)
                        })
            }
            _ => false,
        },
        _ => false,
    }
}

#[derive(Clone)]
pub(crate) struct DecodedImage {
    pub(crate) width: i32,
    pub(crate) height: i32,
    pub(crate) pixels: Vec<u8>,
}

impl DecodedImage {
    pub(crate) fn load(path: &str) -> Result<Self, ImageLoadError> {
        let file = std::fs::File::open(path)
            .map_err(|e| ImageLoadError::Other(format!("failed to open image {path}: {e}")))?;
        let stat = file.metadata().ok();
        let image = image_reader(&file, path)
            .map_err(|e| {
                ImageLoadError::Other(format!("failed to detect image format for {path}: {e}"))
            })?
            .decode()
            .map_err(|error| {
                let message = format!("failed to decode image {path}: {error}");
                if let Some(stat) = &stat
                    && stat.is_file()
                    && rejected_source(&error, &file, path)
                {
                    ImageLoadError::Invalid(mural_ipc::InvalidImage {
                        path: path.to_owned(),
                        version: mural_core::wallpaper::image_version(stat),
                        message,
                    })
                } else {
                    ImageLoadError::Other(message)
                }
            })?
            .into_rgba8();

        let width = i32::try_from(image.width()).map_err(|_| {
            ImageLoadError::Other(format!("image width is too large for GL: {path}"))
        })?;
        let height = i32::try_from(image.height()).map_err(|_| {
            ImageLoadError::Other(format!("image height is too large for GL: {path}"))
        })?;
        if width <= 0 || height <= 0 {
            return Err(ImageLoadError::Other(format!(
                "image has invalid dimensions: {path}"
            )));
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
    pub(crate) library_target: bool,
    pub(crate) id: u64,
    pub(crate) image_path: String,
}

pub(crate) struct DecodeResult {
    pub(crate) library_target: bool,
    pub(crate) id: u64,
    pub(crate) result: Result<DecodedImage, ImageLoadError>,
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
        if result_tx
            .send(DecodeResult {
                id: job.id,
                library_target: job.library_target,
                result,
            })
            .is_err()
        {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn content_and_declared_size_are_not_io_or_allocator_failures() {
        let path =
            std::env::temp_dir().join(format!("mural-decode-rejection-{}.png", std::process::id()));
        let name = path.to_str().unwrap();
        fs::write(&path, b"not an image").unwrap();
        assert!(matches!(
            DecodedImage::load(name),
            Err(ImageLoadError::Invalid(_))
        ));
        // Synthetic 20,000 x 10,000 RGB header: the output budget is checked before pixel inflation.
        fs::write(&path, HEADER).unwrap();
        let Err(ImageLoadError::Invalid(image)) = DecodedImage::load(name) else {
            panic!("expected decoder budget rejection");
        };
        assert!(image.message.contains("Memory limit exceeded"));
        image::RgbImage::new(2, 2).save(&path).unwrap();
        assert!(DecodedImage::load(name).is_ok());
        let file = fs::File::open(&path).unwrap();
        let low_memory = image::ImageError::Limits(image::error::LimitError::from_kind(
            image::error::LimitErrorKind::InsufficientMemory,
        ));
        assert!(!rejected_source(&low_memory, &file, name));
        assert!(!rejected_source(
            &image::ImageError::IoError(std::io::ErrorKind::PermissionDenied.into()),
            &file,
            name
        ));
        fs::remove_file(&path).unwrap();
        assert!(matches!(
            DecodedImage::load(name),
            Err(ImageLoadError::Other(_))
        ));
    }

    const HEADER: &[u8] = &[
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x4e, 0x20, 0x00, 0x00, 0x27, 0x10, 0x08, 0x02, 0x00, 0x00, 0x00, 0x76,
        0x46, 0xdf, 0xf5, 0x00, 0x00, 0x00, 0x08, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x03, 0x00,
        0x00, 0x00, 0x00, 0x01, 0x48, 0x06, 0x89, 0xd2, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e,
        0x44, 0xae, 0x42, 0x60, 0x82,
    ];
}
