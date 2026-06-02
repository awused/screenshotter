// TODO -- maybe skip this?

use image::RgbImage;

use crate::util::{LFRegion, MRegion};

#[derive(Debug)]
pub struct Screenshot {
    pub image: RgbImage,
    pub logical: LFRegion,
    pub scale: f64,
}

pub enum ImageData {
    // We can leave data as bgra until the last moment
    Bgra(Vec<u8>),
    Rgb(Vec<u8>),
}

pub struct Image {
    pub data: ImageData,
    pub res: (u32, u32),
}

// impl fmt::Debug for Image {
//     fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
//         write!(f, "[img: {}-{:?}]", self.data.format(), self.res)
//     }
// }
