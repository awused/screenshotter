use image::RgbImage;

use crate::config::CONFIG;
use crate::img::resample::resize_par_linear;
use crate::util::{LFRegion, MRegion, Monitor, ORegion, Overlap};

mod resample;

#[derive(Debug)]
pub struct Screenshot {
    pub image: RgbImage,
    pub logical: LFRegion,
    pub monitor_region: MRegion,
    pub monitor: Monitor,
}

impl Screenshot {
    pub fn output_region_unscaled(&self) -> ORegion {
        println!("{}", self.monitor.description);
        let (x, y) = if let Some(pos) = CONFIG
            .monitor_positions
            .iter()
            .find(|p| self.monitor.description.contains(&p.id))
        {
            (pos.x + self.monitor_region.x, pos.y + self.monitor_region.y)
        } else {
            let x = (self.logical.x + 0.5).floor() as i32;
            let y = (self.logical.y + 0.5).floor() as i32;

            (x, y)
        };

        ORegion {
            x,
            y,
            width: self.image.width() as _,
            height: self.image.height() as _,
        }
    }
}

fn fix_overlaps(regions: &mut [ORegion]) {
    'outer: loop {
        for i in 0..regions.len() - 1 {
            for j in i + 1..regions.len() {
                match regions[i].overlap(&regions[j]) {
                    Overlap::Nothing => continue,
                    Overlap::X(x) => {
                        info!("Corrected horizontal overlap of {x}");
                        if regions[i].x <= regions[j].x {
                            regions[j].x += x;
                        } else {
                            regions[i].x += x;
                        }
                    }
                    Overlap::Y(y) => {
                        info!("Corrected vertical overlap of {y}");
                        if regions[i].y <= regions[j].y {
                            regions[j].y += y;
                        } else {
                            regions[i].y += y;
                        }
                    }
                }
                continue 'outer;
            }
        }

        break;
    }
}

pub fn combine(shots: Vec<Screenshot>, scale_up: bool) -> RgbImage {
    assert!(!shots.is_empty(), "Cannot produce final image if there are no screenshots");

    let mut max_scale = shots[0].monitor.fixed_scale();

    for s in &shots[1..] {
        if s.monitor.fixed_scale() > max_scale {
            max_scale = s.monitor.fixed_scale();
        }
    }

    let mut output_regions: Vec<_> = shots
        .iter()
        .map(|s| {
            if scale_up {
                s.logical.output_region(max_scale)
            } else {
                s.output_region_unscaled()
            }
        })
        .collect();

    let min_x = output_regions.iter().map(|o| o.x).min().unwrap();
    let min_y = output_regions.iter().map(|o| o.y).min().unwrap();

    for (out, shot) in output_regions.iter_mut().zip(&shots) {
        // If this is ever off we have a problem
        if (shot.monitor.fixed_scale() == max_scale || !scale_up)
            && (out.width as u32, out.height as u32) != shot.image.dimensions()
        {
            // This can happen when scales don't match, and we get a position that is logically
            // crossing inside a pixel. Output region rounds up, so there'll be a 1px gap somewhere
            // instead of overlap.
            warn!(
                "Logical captured region crossed inside a pixel. Expected {:?}, got {:?}. There \
                 will be a gap.",
                (out.width, out.height),
                shot.image.dimensions()
            );
            if shots.len() == 1 {
                out.width = shot.image.width() as _;
                out.height = shot.image.height() as _;
            }
        }

        assert!(out.width > 0);
        assert!(out.height > 0);

        // Repage
        out.x -= min_x;
        out.y -= min_y;
    }

    fix_overlaps(&mut output_regions);

    let width = output_regions.iter().map(|o| o.x + o.width).max().unwrap();
    let height = output_regions.iter().map(|o| o.y + o.height).max().unwrap();

    assert!(width > 0);
    assert!(height > 0);

    let mut out = RgbImage::new(width as _, height as _).into_vec();

    let out_stride = width as usize * 3;

    for (region, shot) in output_regions.into_iter().zip(shots) {
        let img = shot.image;

        let (width, img) = if scale_up && shot.monitor.fixed_scale() != max_scale {
            let current = (img.width(), img.height());

            // TODO -- resample directly into the final buffer, skipping the de-rotate in
            // vertical_par_sample
            (
                region.width as usize,
                resize_par_linear::<3>(
                    &img.into_vec(),
                    current,
                    (region.width as u32, region.height as u32),
                    // Lanczos3 is sharper, nicer for upscaling, but without an anti-ringing filter
                    // it's annoying.
                    resample::FilterType::CatmullRom,
                ),
            )
        } else {
            (img.width() as usize, img.into_vec())
        };

        assert!(img.len() <= region.width as usize * region.height as usize * 3);

        let out_start = region.y as usize * out_stride + region.x as usize * 3;
        let len = width * 3;


        // Opportunity for fast paths here
        for y in 0..region.height as usize {
            let out_start = out_start + y * out_stride;
            out[out_start..out_start + len].copy_from_slice(&img[y * len..y * len + len]);
        }
        // region
    }

    RgbImage::from_raw(width as _, height as _, out).unwrap()
}
