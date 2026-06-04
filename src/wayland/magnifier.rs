use core::slice;
use std::rc::Rc;

use color_eyre::Result;
use color_eyre::eyre::bail;
use wayland_client::protocol::wl_buffer::WlBuffer;
use wayland_client::protocol::wl_callback::WlCallback;
use wayland_client::protocol::wl_subsurface::WlSubsurface;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{Dispatch, NoopIgnore, QueueHandle};
use wayland_protocols::wp::viewporter::client::wp_viewport::WpViewport;

use crate::util::{MLPoint, Monitor};
use crate::wayland::buffer::Buffer;
use crate::wayland::protos::Protos;
use crate::wayland::{OutputKey, State};

// Must be odd
const RES: usize = 11;
const SCALE: usize = 20;
const LARGE_RES: usize = RES * SCALE;
const PADDING: i32 = 30;

struct MagnifierViewKey(OutputKey, usize);
struct MagnifierKey(OutputKey);

#[derive(Debug)]
struct View {
    locked: bool,
    buffer: Buffer,
    origin: (i32, i32),
}

#[derive(Debug)]
pub struct Magnifier {
    protos: Rc<Protos>,
    output: OutputKey,
    monitor: Monitor,

    zoom_surface: WlSurface,
    zoom_subsurface: WlSubsurface,
    zoom_viewport: WpViewport,

    crosshair_surface: WlSurface,
    crosshair_subsurface: WlSubsurface,
    crosshair_viewport: WpViewport,

    pending: Option<MLPoint>,
    views: Vec<View>,
    last_view: Option<usize>,

    zoom_frame: Option<WlCallback>,

    frozen: Rc<Buffer>,
    cross: Rc<Buffer>,
}

impl Magnifier {
    pub fn hide(&mut self) {
        trace!("Hiding magnifier");
        self.last_view = None;
        self.pending = None;
        self.zoom_frame = None;

        self.zoom_surface.attach(None, 0, 0);
        self.crosshair_surface.attach(None, 0, 0);
        self.zoom_surface.commit();
        self.crosshair_surface.commit();
    }

    pub fn draw(&mut self, qh: &QueueHandle<State>, point: MLPoint) -> Result<bool> {
        if self.zoom_frame.is_some() {
            self.pending = Some(point);
            return Ok(false);
        }

        let origin = self.monitor.local_pixel(point);

        let (index, view) = if let Some(view) =
            self.views.iter_mut().enumerate().find(|(_, v)| !v.locked && v.origin == origin)
        {
            view
        } else if let Some(view) = self.views.iter_mut().enumerate().find(|(_, v)| !v.locked) {
            view
        } else {
            let index = self.views.len();
            if index > 3 {
                bail!("Too many magnifier views, they're not being unlocked");
            }
            debug!("Creating magnifier {index} for {:?}", self.output);

            let buffer = Buffer::new(
                &self.protos,
                qh,
                self.frozen.format,
                LARGE_RES as _,
                LARGE_RES as _,
                MagnifierViewKey(self.output, index),
            )?;
            (index, self.views.push_mut(View { buffer, origin: (-1, -1), locked: false }))
        };
        view.locked = true;

        if origin != view.origin {
            view.origin = origin;
            // SAFETY: we checked it was unlocked or newly created it
            unsafe {
                view.draw(&self.frozen, &self.monitor);
            }
        }

        if self.last_view.is_none() {
            self.crosshair_surface.attach(Some(&self.cross.wl_buffer), 0, 0);
            self.crosshair_surface.damage(0, 0, LARGE_RES as _, LARGE_RES as _);
        }

        self.zoom_surface.attach(Some(&view.buffer.wl_buffer), 0, 0);
        self.position(point);

        self.zoom_surface.damage(0, 0, LARGE_RES as _, LARGE_RES as _);
        self.zoom_frame = Some(self.zoom_surface.frame(qh, MagnifierKey(self.output)));

        self.crosshair_surface.commit();
        self.zoom_surface.commit();

        self.last_view = Some(index);

        Ok(true)
    }

    fn position(&self, MLPoint { x, y }: MLPoint) {
        let log_x = x.round() as i32;
        let log_y = y.round() as i32;

        let mut pos_x = log_x + PADDING;
        if pos_x + LARGE_RES as i32 >= self.monitor.logical.width {
            pos_x = log_x - LARGE_RES as i32 - PADDING;
        }

        let mut pos_y = log_y - LARGE_RES as i32 - PADDING;
        if pos_y < 0 {
            pos_y = log_y + PADDING;
        }

        self.zoom_subsurface.set_position(pos_x, pos_y);
        self.crosshair_subsurface.set_position(pos_x, pos_y);
    }

    #[instrument(level = "error", skip_all)]
    pub fn new(
        protos: &Rc<Protos>,
        qh: &QueueHandle<State>,
        output: OutputKey,
        monitor: &Monitor,
        freeze_surface: &WlSurface,
        frozen: &Rc<Buffer>,
        cross: &Rc<Buffer>,
    ) -> Result<Self> {
        let zoom_surface = protos.compositor().create_surface(qh, NoopIgnore);
        let region = protos.compositor().create_region(qh, NoopIgnore);
        zoom_surface.set_input_region(Some(&region));

        let zoom_subsurface =
            protos
                .subcompositor()
                .get_subsurface(&zoom_surface, freeze_surface, qh, NoopIgnore);

        let zoom_viewport = protos.viewporter().get_viewport(&zoom_surface, qh, NoopIgnore);

        let crosshair_surface = protos.compositor().create_surface(qh, NoopIgnore);
        crosshair_surface.set_input_region(Some(&region));
        region.destroy();

        let crosshair_subsurface = protos.subcompositor().get_subsurface(
            &crosshair_surface,
            freeze_surface,
            qh,
            NoopIgnore,
        );

        let crosshair_viewport =
            protos.viewporter().get_viewport(&crosshair_surface, qh, NoopIgnore);

        Ok(Self {
            protos: protos.clone(),
            output,
            monitor: monitor.clone(),

            zoom_surface,
            zoom_subsurface,
            zoom_viewport,

            crosshair_surface,
            crosshair_subsurface,
            crosshair_viewport,

            pending: None,
            views: Vec::new(),
            last_view: None,

            zoom_frame: None,

            frozen: frozen.clone(),
            cross: cross.clone(),
        })
    }
}

impl View {
    // Cannot be run between commit() and the release
    unsafe fn draw(&mut self, frozen: &Buffer, monitor: &Monitor) {
        let bytes = frozen.format.size();
        let frozen_stride = bytes * frozen.width;
        let stride = bytes * self.buffer.width;

        let inset = RES as i32 / 2;

        // Could slightly optimizing drawing off-monitor area but probably not worth it
        unsafe {
            for y in 0..RES as i32 {
                for x in 0..RES as i32 {
                    let true_x = x - inset + self.origin.0;
                    let true_y = y - inset + self.origin.1;
                    let (true_x, true_y) = monitor.transform.correct(
                        (true_x, true_y),
                        (monitor.physical.width, monitor.physical.height),
                    );

                    let pixel = if true_x >= 0
                        && (true_x as usize) < frozen.width
                        && true_y >= 0
                        && (true_y as usize) < frozen.height
                    {
                        let offset = (true_y as usize) * frozen_stride + (true_x as usize) * bytes;
                        debug_assert!(offset + bytes <= frozen.buf_size);
                        slice::from_raw_parts(frozen.buf.cast::<u8>().add(offset), bytes)
                    } else {
                        // Works for rgba and bgr
                        &[0, 0, 0, 255][0..bytes]
                    };

                    let start = (y as usize * stride + x as usize * bytes) * SCALE;
                    for j in 0..SCALE {
                        for i in 0..SCALE {
                            let out = start + j * stride + i * bytes;
                            debug_assert!(out + bytes <= self.buffer.buf_size);
                            slice::from_raw_parts_mut(self.buffer.buf.cast::<u8>().add(out), bytes)
                                .copy_from_slice(pixel);
                        }
                    }
                }
            }
        }
    }
}


pub fn draw_crosshair(state: &State, qhandle: &QueueHandle<State>) -> Result<Buffer> {
    let format = state.transparent_format();
    let buffer =
        Buffer::new(&state.protos, qhandle, format, LARGE_RES as _, LARGE_RES as _, NoopIgnore)?;

    let stride = format.size() * LARGE_RES;
    let size = stride * LARGE_RES;

    // TODO -- support non-argb8888 formats. HDR10 has minimal transparency
    let mut drawing = vec![0u8; size];
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

    unsafe {
        assert_eq!(size, buffer.buf_size);
        buffer.buf.copy_from_nonoverlapping(drawing.as_ptr().cast(), size);
    }

    Ok(buffer)
}

impl Dispatch<WlBuffer, State> for MagnifierViewKey {
    fn event(
        &self,
        state: &mut State,
        _proxy: &WlBuffer,
        _event: <WlBuffer as wayland_client::Proxy>::Event,
        _conn: &wayland_client::Connection,
        _qhandle: &QueueHandle<State>,
    ) {
        state
            .outputs
            .get_mut(&self.0)
            .unwrap()
            .overlay
            .get_mut()
            .unwrap()
            .magnifier
            .get_mut()
            .unwrap()
            .views[self.1]
            .locked = false;
    }
}

impl Dispatch<WlCallback, State> for MagnifierKey {
    fn event(
        &self,
        state: &mut State,
        _proxy: &WlCallback,
        _event: <WlCallback as wayland_client::Proxy>::Event,
        _conn: &wayland_client::Connection,
        qh: &QueueHandle<State>,
    ) {
        state.try_handle(|state| {
            let overlay = state.outputs.get_mut(&self.0).unwrap().overlay.get_mut().unwrap();
            let mag = overlay.magnifier.get_mut().unwrap();

            mag.zoom_frame.take();
            if let Some(point) = mag.pending.take() {
                overlay.move_magnifier(qh, point)?;
            }

            Ok(())
        });
    }
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
