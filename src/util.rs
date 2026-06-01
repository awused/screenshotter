use std::cmp::{max, min};
use std::fmt::Display;

use color_eyre::eyre::OptionExt;
use serde::Serialize;

use crate::ipc::Window;
use crate::wayland::Transform;

// A region contains the points (x, y) and (x + width - 1, y + height - 1)
// x + width is the column just past the edge of the window


#[derive(Debug, PartialEq, Clone, Copy)]
pub struct SelectPoint {
    logical: LPoint,
    precise: MLPoint,
}

// A point in global logical space.
#[derive(Debug, PartialEq, Clone, Copy, Default)]
pub struct LPoint {
    pub x: f64,
    pub y: f64,
}

// A point in monitor-local logical pixels. Meaningless without a monitor's region.
#[derive(Default, Debug, PartialEq, Clone, Copy)]
pub struct MLPoint {
    pub x: f64,
    pub y: f64,
}

// A region in global logical float pixels
// The underlying representation in wayland is 1/256 fixed point
#[derive(Debug, Clone, Copy)]
pub struct LFRegion {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

// A region in global logical pixels
// TODO -- consider dropping this for LFReion everywhere
#[derive(Debug, Eq, PartialEq, Clone, Copy, Serialize, Default)]
pub struct LRegion {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

#[derive(Debug, PartialEq, Clone, Copy, Default)]
pub struct Monitor {
    pub logical: LRegion,
    pub physical: MRegion,
    pub scale: f64,
    pub transform: Transform,
}

// A region in monitor-local pixels, after applying scales. (0, 0) is the top left corner of the
// monitor after applying transformations.
#[derive(Debug, Eq, PartialEq, Clone, Copy, Default)]
pub struct MRegion {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

// A region in output coordinates.
#[derive(Debug, Eq, PartialEq, Clone, Copy, Default)]
pub struct ORegion {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl Monitor {
    pub fn global_pixel_bounds(&self, point: MLPoint) -> LFRegion {
        // top left to bottom right of the containing pixel
        let x = (point.x * self.scale).floor() / self.scale + self.logical.x as f64;
        let y = (point.y * self.scale).floor() / self.scale + self.logical.y as f64;

        LFRegion {
            x,
            y,
            width: self.scale,
            height: self.scale,
        }
    }

    pub fn local_to_global(&self, point: MLPoint) -> LPoint {
        let x = self.logical.x as f64 + point.x;
        let y = self.logical.y as f64 + point.y;
        LPoint { x, y }
    }

    pub fn intersect_float(&self, other: &LFRegion) -> Option<MRegion> {
        other.intersect(&self.logical.into()).and_then(|r| {
            let left = ((r.x - self.logical.x as f64) * self.scale).floor() as _;
            let top = ((r.y - self.logical.y as f64) * self.scale).floor() as _;
            let right = ((r.x - self.logical.x as f64 + r.width) * self.scale).ceil() as i32;
            let bottom = ((r.y - self.logical.y as f64 + r.height) * self.scale).ceil() as i32;

            let width = right - left;
            let height = bottom - top;

            if left < 0 || right > self.physical.width || top < 0 || bottom > self.physical.height {
                error!("Bad intersection {self:?}, {left},{top} {width}x{height}");
                None
            } else {
                Some(MRegion { x: left, y: top, width, height })
            }
        })
    }

    pub fn intersect(&self, other: &LRegion) -> Option<MRegion> {
        self.logical.intersect(other).and_then(|r| {
            // TODO -- ensure this can't
            let left = ((r.x - self.logical.x) as f64 * self.scale).floor() as _;
            let top = ((r.y - self.logical.y) as f64 * self.scale).floor() as _;
            let right = ((r.x - self.logical.x + r.width) as f64 * self.scale).ceil() as i32;
            let bottom = ((r.y - self.logical.y + r.height) as f64 * self.scale).ceil() as i32;

            let width = right - left;
            let height = bottom - top;

            if left < 0 || right > self.physical.width || top < 0 || bottom > self.physical.height {
                error!("Bad intersection {self:?}, {left},{top} {width}x{height}");
                None
            } else {
                Some(MRegion { x: left, y: top, width, height })
            }
        })
    }
}

impl LFRegion {
    pub fn upper_left(&self) -> LPoint {
        LPoint { x: self.x, y: self.y }
    }

    pub fn intersect(self, other: &Self) -> Option<Self> {
        let left = f64::max(self.x, other.x);
        let right = f64::min(self.x + self.width, other.x + other.width);
        let top = f64::max(self.y, other.y);
        let bottom = f64::min(self.y + self.height, other.y + other.height);
        if right > left && bottom > top {
            Some(Self {
                x: left,
                y: top,
                width: right - left,
                height: bottom - top,
            })
        } else {
            None
        }
    }

    // Given precise points on two monitors, find a logical bounding region that definitely
    // contains both points.
    // pub fn bounding_region(
    //     a: MLPoint,
    //     b: MLPoint,
    //     monitor_a: &Monitor,
    //     monitor_b: &Monitor,
    // ) -> Self {
    // monitor_a.local_to_global(a).bounding_region(&monitor_b.local_to_global(b))
    // }

    pub fn bounding_region(&self, other: &Self) -> Self {
        let left = f64::min(self.x, other.x);
        let right = f64::max(self.x + self.width, other.x + other.width);
        let top = f64::min(self.y, other.y);
        let bottom = f64::max(self.y + self.height, other.y + other.height);
        Self {
            x: left,
            y: top,
            width: right - left,
            height: bottom - top,
        }
    }

    pub fn int_region(&self) -> LRegion {
        let left = self.x.floor() as i32;
        let right = (self.x + self.width).ceil() as i32;
        let top = self.y.floor() as i32;
        let bottom = (self.y + self.height).ceil() as i32;
        LRegion {
            x: left,
            y: top,
            width: right - left,
            height: bottom - top,
        }
    }

    pub fn best_window(&self, windows: &mut Vec<Window>) -> Option<Window> {
        let x = self.x + self.width / 2.;
        let y = self.y + self.height / 2.;
        let point = LPoint { x, y };
        windows.extract_if(.., |w| w.region().contains(point)).next()
    }
}

impl LRegion {
    pub const fn contains(&self, point: LPoint) -> bool {
        self.x as f64 <= point.x
            && (self.x + self.width) as f64 > point.x
            && self.y as f64 <= point.y
            && (self.y + self.height) as f64 > point.y
    }

    // Returns a non-empty intersection
    pub fn intersect(self, other: &Self) -> Option<Self> {
        let left = max(self.x, other.x);
        let right = min(self.x + self.width, other.x + other.width);
        let top = max(self.y, other.y);
        let bottom = min(self.y + self.height, other.y + other.height);
        if right > left && bottom > top {
            Some(Self {
                x: left,
                y: top,
                width: right - left,
                height: bottom - top,
            })
        } else {
            None
        }
    }

    // This should only be necessary when the selection crosses boundaries.
    pub fn monitor_intersect(self, m: &Monitor) -> Option<(Self, MRegion)> {
        let logical = self.intersect(&m.logical)?;
        let x = ((logical.x - m.logical.x) as f64 * m.scale).floor() as _;
        let y = ((logical.y - m.logical.y) as f64 * m.scale).floor() as _;
        let width = (logical.width as f64 * m.scale).ceil() as _;
        let height = (logical.height as f64 * m.scale).ceil() as _;

        Option::Some((logical, MRegion { x, y, width, height }))
    }

    pub const fn is_empty(&self) -> bool {
        self.width == 0 || self.height == 0
    }
}

impl MRegion {
    pub const fn is_empty(&self) -> bool {
        self.width == 0 || self.height == 0
    }
}

impl LPoint {
    pub fn bounding_region(&self, other: &Self) -> LFRegion {
        let Self { x: x1, y: y1 } = *self;
        let Self { x: x2, y: y2 } = *other;

        let left = f64::min(x1, x2);
        let right = f64::max(x1, x2);
        let top = f64::min(y1, y2);
        let bottom = f64::max(y1, y2);

        LFRegion {
            x: left,
            y: top,
            width: right - left + 1.0,
            height: bottom - top + 1.0,
        }
    }
}


impl Display for LRegion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{},{} {}x{}", self.x, self.y, self.width, self.height)
    }
}

impl TryFrom<String> for LRegion {
    type Error = color_eyre::Report;

    fn try_from(input: String) -> std::prelude::v1::Result<Self, Self::Error> {
        let input = input.trim();
        let (x, rest) = input.split_once(',').ok_or_eyre(format!("Invalid region {input}"))?;
        let (y, rest) = rest.split_once(' ').ok_or_eyre(format!("Invalid region {input}"))?;
        let (width, height) = rest.split_once('x').ok_or_eyre(format!("Invalid region {input}"))?;

        Ok(Self {
            x: x.parse()?,
            y: y.parse()?,
            width: width.parse()?,
            height: height.parse()?,
        })
    }
}

impl From<LRegion> for LFRegion {
    fn from(r: LRegion) -> Self {
        Self {
            x: r.x as f64,
            y: r.y as f64,
            width: r.width as f64,
            height: r.height as f64,
        }
    }
}
