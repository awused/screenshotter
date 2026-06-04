use std::cmp::{max, min};
use std::fmt::Display;
use std::rc::Rc;

use color_eyre::eyre::OptionExt;
use serde::Serialize;

use crate::ipc::Window;
use crate::wayland::Transform;

// A region contains the points (x, y) and (x + width - 1, y + height - 1)
// x + width is the column just past the edge of the window

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

// A region in global logical pixels.
// Only comes into the application from the IPC interface.
#[derive(Debug, Eq, PartialEq, Clone, Copy, Serialize, Default)]
pub struct LRegion {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

#[derive(Debug, PartialEq, Clone, Default)]
pub struct Monitor {
    pub logical: LRegion,
    pub physical: MRegion,
    pub scale: f64,
    pub transform: Transform,
    pub description: Rc<str>,
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
    // Scale as a fixed point with denominator 120
    pub fn fixed_scale(&self) -> u32 {
        (self.scale * 120.).round() as u32
    }

    pub fn global_pixel_bounds(&self, point: MLPoint) -> LFRegion {
        // top left to bottom right of the containing pixel
        let x = (point.x * self.scale).floor() / self.scale + self.logical.x as f64;
        let y = (point.y * self.scale).floor() / self.scale + self.logical.y as f64;

        LFRegion {
            x,
            y,
            width: 1. / self.scale,
            height: 1. / self.scale,
        }
    }

    pub fn local_pixel(&self, point: MLPoint) -> (i32, i32) {
        ((point.x * self.scale).floor() as _, (point.y * self.scale).floor() as _)
    }

    pub fn local_to_global(&self, point: MLPoint) -> LPoint {
        let x = self.logical.x as f64 + point.x;
        let y = self.logical.y as f64 + point.y;
        LPoint { x, y }
    }

    // Exists solely to handle drag events
    pub fn global_to_local(&self, point: LPoint) -> MLPoint {
        let x = point.x - self.logical.x as f64;
        let y = point.y - self.logical.y as f64;
        MLPoint { x, y }
    }

    // LFRegions are pixel-perfect if everything is at the same scale, so reduce tiny variations
    // with rounding instead of potentially amplifying them.
    //
    // If everything is not at the same scale, everything is best-effort.
    pub fn intersect_rounded(&self, other: &LFRegion) -> Option<(LFRegion, MRegion)> {
        other.intersect(&self.logical.into()).and_then(|r| {
            let left = ((r.x - self.logical.x as f64) * self.scale).round() as _;
            let top = ((r.y - self.logical.y as f64) * self.scale).round() as _;
            let right = ((r.x - self.logical.x as f64 + r.width) * self.scale).round() as i32;
            let bottom = ((r.y - self.logical.y as f64 + r.height) * self.scale).round() as i32;

            let width = right - left;
            let height = bottom - top;

            if left < 0 || right > self.physical.width || top < 0 || bottom > self.physical.height {
                error!("Bad intersection {self:?}, {left},{top} {width}x{height}");
                None
            } else {
                Some((r, MRegion { x: left, y: top, width, height }))
            }
        })
    }
}

impl LFRegion {
    pub const fn output_region(&self, scale: u32) -> ORegion {
        let x = ((self.x * scale as f64 + 60.) / 120.).floor() as i32;
        let y = ((self.y * scale as f64 + 60.) / 120.).floor() as i32;
        let right = (((self.x + self.width) * scale as f64 + 60.) / 120.).floor() as i32;
        let bottom = (((self.y + self.height) * scale as f64 + 60.) / 120.).floor() as i32;
        ORegion {
            x,
            y,
            width: right - x,
            height: bottom - y,
        }
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

    // We couldn't find a perfect match, so add a 0.5px margin
    // Don't do this first in case it would cause overlaps.
    pub const fn contains_lenient(&self, point: LPoint) -> bool {
        self.x as f64 <= point.x + 0.5
            && (self.x + self.width) as f64 > point.x - 0.5
            && self.y as f64 <= point.y + 0.5
            && (self.y + self.height) as f64 > point.y - 0.5
    }

    pub const fn valid_mouse(&self, point: MLPoint) -> bool {
        point.x >= 0.0
            && point.x < self.width as f64
            && point.y >= 0.0
            && point.y < self.height as f64
    }

    #[cfg(feature = "hyprland")]
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

    pub const fn is_empty(&self) -> bool {
        self.width == 0 || self.height == 0
    }
}

impl MRegion {
    pub const fn is_empty(&self) -> bool {
        self.width == 0 || self.height == 0
    }
}

#[derive(Debug)]
pub enum Overlap {
    Nothing,
    X(i32),
    Y(i32),
}

impl ORegion {
    // Greedy, prioritize the smallest movement. self should also be at a lower index than other in
    // the list, so prioritize moving other just for consistency.
    pub fn overlap(&self, other: &Self) -> Overlap {
        let left = max(self.x, other.x);
        let right = min(self.x + self.width, other.x + other.width);
        let top = max(self.y, other.y);
        let bottom = min(self.y + self.height, other.y + other.height);

        if right > left && bottom > top {
            let x = if self.x <= other.x {
                self.x + self.width - other.x
            } else {
                other.x + other.width - self.x
            };

            let y = if self.y <= other.y {
                self.y + self.height - other.y
            } else {
                other.y + other.height - self.y
            };
            if x <= y { Overlap::X(x) } else { Overlap::Y(y) }
        } else {
            Overlap::Nothing
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
