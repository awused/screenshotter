use std::cmp::{max, min};
use std::fmt::Display;

use color_eyre::eyre::OptionExt;
use serde::Serialize;

use crate::ipc::Window;

#[derive(Debug, Eq, PartialEq, Clone, Copy)]
pub struct SelectPoint {
    logical: LPoint,
    precise: MPoint,
}

// A point in global logical space.
#[derive(Debug, Eq, PartialEq, Clone, Copy, Default)]
pub struct LPoint {
    pub x: i32,
    pub y: i32,
}

// A point in monitor-local space. Meaningless without a monitor's region.
#[derive(Debug, Eq, PartialEq, Clone, Copy)]
pub struct MPoint {
    pub x: i32,
    pub y: i32,
}

// A region in global logical pixels
#[derive(Debug, Eq, PartialEq, Clone, Copy, Serialize, Default)]
pub struct LRegion {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

#[derive(Debug, PartialEq, Clone, Copy, Default)]
pub struct Monitor {
    pub region: LRegion,
    pub scale: f64,
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
    pub fn precise_logical(&self, point: MPoint) -> (f64, f64) {
        let x = self.region.x as f64 + (point.x as f64 / self.scale);
        let y = self.region.y as f64 + (point.y as f64 / self.scale);
        (x, y)
    }
}

impl LRegion {
    const fn contains(&self, point: LPoint) -> bool {
        self.x <= point.x
            && self.x + self.width >= point.x
            && self.y <= point.y
            && self.y + self.height >= point.y
    }

    // TODO -- remove the part about exact matches
    pub fn best_window(&self, mut windows: Vec<Window>) -> Option<Window> {
        // Priotitize exact matches, even if the center is somewhere else
        if let Some(exact) = windows.extract_if(.., |w| *self == w.region()).next() {
            return Some(exact);
        }

        let x = self.x + self.width / 2;
        let y = self.y + self.height / 2;
        let point = LPoint { x, y };
        windows.into_iter().find(|w| w.region().contains(point))
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
        let logical = self.intersect(&m.region)?;
        let x = ((logical.x - m.region.x) as f64 * m.scale).floor() as _;
        let y = ((logical.y - m.region.y) as f64 * m.scale).floor() as _;
        let width = (logical.width as f64 * m.scale).ceil() as _;
        let height = (logical.height as f64 * m.scale).ceil() as _;

        Option::Some((logical, MRegion { x, y, width, height }))
    }

    pub const fn is_empty(&self) -> bool {
        self.width == 0 || self.height == 0
    }

    // Given precise points on two monitors, find a logical bounding region that definitely
    // contains both points.
    pub fn bounding_region(a: MPoint, b: MPoint, monitor_a: &Monitor, monitor_b: &Monitor) -> Self {
        let (x1, y1) = monitor_a.precise_logical(a);
        let (x2, y2) = monitor_b.precise_logical(b);

        let left = f64::min(x1, x2).floor() as i32;
        let right = f64::max(x1, x2).ceil() as i32;
        let top = f64::min(y1, y2).floor() as i32;
        let bottom = f64::max(y1, y2).ceil() as i32;

        Self {
            x: left,
            y: top,
            width: right - left,
            height: bottom - top,
        }
    }
}

impl MRegion {
    pub const fn is_empty(&self) -> bool {
        self.width == 0 || self.height == 0
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
