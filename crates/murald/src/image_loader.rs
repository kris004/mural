use crate::decode::{DecodedImage, ImageLoadError};

pub(crate) fn load(path: &str) -> Result<DecodedImage, ImageLoadError> {
    DecodedImage::load(path)
}
