use crate::decode::DecodedImage;

pub(crate) fn load(path: &str) -> Result<DecodedImage, String> {
    DecodedImage::load(path)
}
