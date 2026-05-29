use std::time::Instant;

use color_eyre::Result;
use wayland_client::protocol::wl_subsurface::WlSubsurface;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{NoopIgnore, QueueHandle};
use wayland_protocols::wp::viewporter::client::wp_viewport::WpViewport;

use crate::util::MRegion;
use crate::wayland::State;
use crate::wayland::protos::{Buffer, Protos};

const RES: usize = 11;
const SCALE: usize = 15;
const LARGE_RES: usize = RES * SCALE;
const PADDING: i32 = 30;

#[derive(Debug)]
pub struct Magnifier {
    pub zoom_surface: WlSurface,
    pub zoom_subsurface: WlSubsurface,
    pub zoom_viewport: WpViewport,

    pub crosshair_surface: WlSurface,
    pub crosshair_subsurface: WlSubsurface,
    pub crosshair_viewport: WpViewport,

    // Whether it's in the drawn state or not
    drawn: bool,
}

impl Magnifier {
    pub fn hide(&self) {
        trace!("Hiding magnifier");
        self.zoom_surface.attach(None, 0, 0);
        self.crosshair_surface.attach(None, 0, 0);
        self.zoom_surface.commit();
        self.crosshair_surface.commit();
    }

    pub fn show(&self, freeze: &Buffer, crosshair: &Buffer) {
        trace!("Displaying magnifier");
        self.zoom_surface.attach(Some(&freeze.wl_buffer), 0, 0);
        self.crosshair_surface.attach(Some(&crosshair.wl_buffer), 0, 0);

        self.zoom_surface.damage(0, 0, LARGE_RES as _, LARGE_RES as _);
        self.crosshair_surface.damage(0, 0, LARGE_RES as _, LARGE_RES as _);
    }

    // TODO[transform]
    pub fn position(&self, x: f64, y: f64, bounds: (u32, u32), monitor: &MRegion) {
        let log_x = x.round() as i32;
        let log_y = y.round() as i32;

        let mut pos_x = log_x + PADDING;
        if pos_x + LARGE_RES as i32 >= bounds.0 as i32 {
            pos_x = log_x - LARGE_RES as i32 - PADDING;
        }

        let mut pos_y = log_y - LARGE_RES as i32 - PADDING;
        if pos_y < 0 {
            pos_y = log_y + PADDING;
        }

        self.zoom_subsurface.set_position(pos_x, pos_y);
        self.crosshair_subsurface.set_position(pos_x, pos_y);

        let scale = monitor.width as f64 / bounds.0 as f64;
        let true_x = (x * scale).trunc() as i32;
        let true_y = (y * scale).trunc() as i32;

        let mut left = true_x - RES as i32 / 2;
        let mut right = true_x + RES as i32 / 2 + 1;
        let mut top = true_y - RES as i32 / 2;
        let mut bottom = true_y + RES as i32 / 2 + 1;

        // Clamp while keeping the center
        if left < 0 {
            right += left;
            left = 0;
        }
        if right >= monitor.width {
            left += right - (monitor.width + 1);
            right = monitor.width - 1;
        }

        if top < 0 {
            bottom += top;
            top = 0;
        }
        if bottom >= monitor.height {
            top += bottom - (monitor.height + 1);
            bottom = monitor.height - 1;
        }
        // TODO -- adjust the output to compensate or don't bother?


        self.zoom_viewport.set_source(
            left as f64,
            top as f64,
            (right - left) as f64,
            (bottom - top) as f64,
        );
        self.zoom_viewport.set_destination(LARGE_RES as i32, LARGE_RES as i32);

        self.zoom_surface.commit();
        self.crosshair_surface.commit();
    }
}

impl Protos {
    #[instrument(level = "error", skip_all)]
    pub fn setup_magnifier(
        &self,
        qh: &QueueHandle<State>,
        freeze_surface: &WlSurface,
    ) -> Result<Magnifier> {
        let zoom_surface = self.compositor().create_surface(qh, NoopIgnore);
        let region = self.compositor().create_region(qh, NoopIgnore);
        zoom_surface.set_input_region(Some(&region));

        let zoom_subsurface =
            self.subcompositor()
                .get_subsurface(&zoom_surface, freeze_surface, qh, NoopIgnore);

        let zoom_viewport = self.viewporter().get_viewport(&zoom_surface, qh, NoopIgnore);

        let crosshair_surface = self.compositor().create_surface(qh, NoopIgnore);
        crosshair_surface.set_input_region(Some(&region));
        region.destroy();

        let crosshair_subsurface =
            self.subcompositor()
                .get_subsurface(&crosshair_surface, freeze_surface, qh, NoopIgnore);

        let crosshair_viewport = self.viewporter().get_viewport(&crosshair_surface, qh, NoopIgnore);

        Ok(Magnifier {
            zoom_surface,
            zoom_subsurface,
            zoom_viewport,

            crosshair_surface,
            crosshair_subsurface,
            crosshair_viewport,

            drawn: false,
        })
    }
}


pub fn draw_crosshair(state: &State, qhandle: &QueueHandle<State>) -> Result<Buffer> {
    let start = Instant::now();
    let format = state.transparent_format();
    let buffer = state.protos.create_buffer(qhandle, format, LARGE_RES as _, LARGE_RES as _)?;

    let stride = format.size() * LARGE_RES;
    let size = stride * LARGE_RES;

    // TODO -- only support u8 for now, eventually we want to consider HDR10 where transparency
    // doesn't matter as much?
    let mut drawing = vec![0u8; size];
    // TODO -- support non-argb8888 formats
    let pixel = [178, 178, 0, 178];
    assert_eq!(pixel.len(), format.size());

    // How many rows/columns skipped to draw the crosshair.
    let inset = (RES / 2) * SCALE;

    for i in 0..RES {
        if i == RES / 2 {
            // Don't draw the center
            continue;
        }

        // These are small boxes, just draw them pixel-by-pixel. One thread is fine?
        for x in 0..SCALE {
            for y in 0..SCALE {
                // Vertical bar
                let start = (y + i * SCALE) * stride + (x + inset) * format.size();
                drawing[start..start + pixel.len()].copy_from_slice(&pixel);

                // Horizontal bar
                let start = (y + inset) * stride + (x + i * SCALE) * format.size();
                drawing[start..start + pixel.len()].copy_from_slice(&pixel);
            }
        }
    }

    // Border
    for i in 0..LARGE_RES {
        let start = i * format.size();
        drawing[start..start + pixel.len()].copy_from_slice(&pixel);
        let start = LARGE_RES * stride - stride + i * format.size();
        drawing[start..start + pixel.len()].copy_from_slice(&pixel);

        let start = i * stride;
        drawing[start..start + pixel.len()].copy_from_slice(&pixel);
        let start = i * stride + stride - format.size();
        drawing[start..start + pixel.len()].copy_from_slice(&pixel);
    }

    // println!("Drew crosshair in {:?}", start.elapsed());

    unsafe {
        assert_eq!(size, buffer.buf_size);
        buffer.buf.copy_from_nonoverlapping(drawing.as_ptr().cast(), size);
    }
    println!("Drew+Copied crosshair in {:?}", start.elapsed());

    Ok(buffer)
}

impl Drop for Magnifier {
    fn drop(&mut self) {
        self.zoom_surface.destroy();
        self.zoom_subsurface.destroy();
        self.zoom_viewport.destroy();

        self.crosshair_surface.destroy();
        self.crosshair_subsurface.destroy();
        self.crosshair_viewport.destroy();
    }
}
