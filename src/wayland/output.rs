use std::cell::OnceCell;
use std::time::Instant;

use color_eyre::eyre::{bail, eyre};
use wayland_client::protocol::wl_output::{self, Transform, WlOutput};
use wayland_client::protocol::wl_subsurface::WlSubsurface;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{Dispatch, NoopIgnore};
use wayland_protocols::ext::image_copy_capture::v1::client::ext_image_copy_capture_frame_v1::{
    self, ExtImageCopyCaptureFrameV1,
};
use wayland_protocols::ext::image_copy_capture::v1::client::ext_image_copy_capture_manager_v1::Options;
use wayland_protocols::ext::image_copy_capture::v1::client::ext_image_copy_capture_session_v1::{
    self, ExtImageCopyCaptureSessionV1,
};
use wayland_protocols::wp::fractional_scale::v1::client::wp_fractional_scale_v1::WpFractionalScaleV1;
use wayland_protocols::wp::viewporter::client::wp_viewport::WpViewport;
use wayland_protocols::xdg::xdg_output::zv1::client::zxdg_output_v1::{self, Event, ZxdgOutputV1};
use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_surface_v1::ZwlrLayerSurfaceV1;

use crate::selection::Region;
use crate::wayland::protos::Buffer;
use crate::wayland::{Format, OutputKey, State};


#[derive(Debug, Default)]
pub struct Capture {
    session: OnceCell<ExtImageCopyCaptureSessionV1>,
    res: OnceCell<(i32, i32)>,
    // Could make this reset to "better" formats for hdr/whatever
    format: OnceCell<Format>,
    frame: OnceCell<ExtImageCopyCaptureFrameV1>,
    transform: OnceCell<Transform>,

    buffer: OnceCell<Buffer>,

    done: bool,
}

#[derive(Debug, Default)]
pub struct Selection {
    layer_surface: OnceCell<ZwlrLayerSurfaceV1>,
    viewport: OnceCell<WpViewport>,

    freeze_surface: OnceCell<WlSurface>,
    select_surface: OnceCell<WlSubsurface>,
    fract_scale: OnceCell<WpFractionalScaleV1>,

    format: OnceCell<Format>,
    res: OnceCell<(i32, i32)>,
    fractional_scale: Option<u32>,

    ready: bool,
}

#[derive(Debug)]
pub struct Output {
    wl_output: WlOutput,
    xdg_output: ZxdgOutputV1,
    region: Region,

    surface: OnceCell<WlSurface>,

    capture: Capture,

    pending_done: usize,
    // All compositors worth caring about implement fractional scale
    fractional_scale: Option<u32>,
    clean: bool,
    dummy_attempted: bool,
}

impl Output {
    pub fn new(wl_output: WlOutput, xdg_output: ZxdgOutputV1) -> Self {
        Self {
            wl_output,
            xdg_output,
            region: Region::default(),

            surface: OnceCell::default(),
            capture: Capture::default(),

            // One for wl_output, one for xdg_output
            pending_done: 2,
            fractional_scale: None,
            clean: false,
            dummy_attempted: false,
        }
    }
}

impl Dispatch<WlOutput, State> for OutputKey {
    fn event(
        &self,
        state: &mut State,
        proxy: &WlOutput,
        event: <WlOutput as wayland_client::Proxy>::Event,
        conn: &wayland_client::Connection,
        qhandle: &wayland_client::QueueHandle<State>,
    ) {
        trace!("WlOutput: {self:?} {event:?}");
        if !matches!(event, wl_output::Event::Done) {
            return;
        }
        state.try_handle(|state| {
            let output = state.outputs.get_mut(self).ok_or_else(|| eyre!("No output {self:?}"))?;
            if output.pending_done == 0 {
                bail!("Got more wl_output::done events than expected. Exiting");
            }
            output.pending_done -= 1;
            if output.pending_done > 0 {
                return Ok(());
            }

            if output.region.is_empty() {
                bail!("Empty region for output {self:?}");
            }

            if state.screenshot {
                // TODO -- allow cursor?
                let source = state.protos.output_capture();
                let source = source.create_source(&output.wl_output, qhandle, NoopIgnore);
                let session = state.protos.image_copy();
                let session = session.create_session(&source, Options::empty(), qhandle, *self);
                source.destroy();

                output
                    .capture
                    .session
                    .set(session)
                    .map_err(|_| eyre!("Output {:?} reconfigured", self))?;
            }

            if state.selection {
                debug!("Selection unimplemented");
            }

            Ok(())
        });
    }
}

impl Dispatch<ZxdgOutputV1, State> for OutputKey {
    fn event(
        &self,
        state: &mut State,
        proxy: &ZxdgOutputV1,
        event: <ZxdgOutputV1 as wayland_client::Proxy>::Event,
        conn: &wayland_client::Connection,
        qhandle: &wayland_client::QueueHandle<State>,
    ) {
        trace!("XdgOutput: {self:?} {event:?}");
        use zxdg_output_v1::Event;

        state.try_handle(|state| {
            let output =
                state.outputs.get_mut(self).ok_or_else(|| eyre!("No output {:?}", self))?;
            let region = &mut output.region;

            match event {
                Event::LogicalPosition { x, y } => {
                    region.x = x;
                    region.y = y;
                }
                Event::LogicalSize { width, height } => {
                    region.width = width;
                    region.height = height;
                }
                Event::Name { .. } | Event::Description { .. } | Event::Done | _ => {}
            }
            Ok(())
        });
    }
}

impl Dispatch<ExtImageCopyCaptureSessionV1, State> for OutputKey {
    fn event(
        &self,
        state: &mut State,
        proxy: &ExtImageCopyCaptureSessionV1,
        event: <ExtImageCopyCaptureSessionV1 as wayland_client::Proxy>::Event,
        conn: &wayland_client::Connection,
        qhandle: &wayland_client::QueueHandle<State>,
    ) {
        trace!("ImageCopyCaptureSession: {self:?} {event:?}");
        use ext_image_copy_capture_session_v1::Event;

        state.try_handle(|state| {
            let output =
                state.outputs.get_mut(self).ok_or_else(|| eyre!("No output {:?}", self))?;
            let capture = &mut output.capture;
            if capture.done {
                return Ok(());
            }

            match event {
                Event::BufferSize { width, height } => {
                    capture
                        .res
                        .set((width.try_into()?, height.try_into()?))
                        .map_err(|_| eyre!("Output {:?} reconfigured", self))?;
                }
                // Argb8888 has to be supported, so nothing else matters.
                // Argb8888 can just be dumped into the overlay directly.
                Event::ShmFormat { format } => {
                    if let Ok(format) = format.try_into() {
                        capture
                            .format
                            .set(format)
                            .map_err(|_| eyre!("Output {:?} had duplicate formats", self))?;
                    }
                }
                Event::Done => {
                    if capture.format.get().is_none() || capture.res.get().is_none() {
                        bail!("No format or resolution for {self:?}: {output:?}");
                    }

                    let frame = proxy.create_frame(qhandle, *self);

                    let (width, height) = *capture.res.get().unwrap();

                    let buffer = state.protos.create_buffer(
                        qhandle,
                        *capture.format.get().unwrap(),
                        width,
                        height,
                    )?;
                    frame.attach_buffer(&buffer.wl_buffer);
                    frame.damage_buffer(0, 0, width as _, height as _);
                    frame.capture();

                    capture
                        .frame
                        .set(frame)
                        .map_err(|_| eyre!("Capture frame reconfigured for {self:?} {output:?}"))?;
                    output.capture.buffer.set(buffer).unwrap();
                }
                Event::Stopped => {
                    bail!("Image copy capture session stopped for {self:?}: {output:?}");
                }
                Event::DmabufFormat { format, modifiers } => {}
                Event::DmabufDevice { .. } | Event::DmabufFormat { .. } | _ => {}
            }
            Ok(())
        })
    }
}

impl Dispatch<ExtImageCopyCaptureFrameV1, State> for OutputKey {
    fn event(
        &self,
        state: &mut State,
        proxy: &ExtImageCopyCaptureFrameV1,
        event: <ExtImageCopyCaptureFrameV1 as wayland_client::Proxy>::Event,
        conn: &wayland_client::Connection,
        qhandle: &wayland_client::QueueHandle<State>,
    ) {
        trace!("ImageCopyCaptureFrame: {self:?} {event:?}");
        use ext_image_copy_capture_frame_v1::Event;

        state.try_handle(|state| {
            let output =
                state.outputs.get_mut(self).ok_or_else(|| eyre!("No output {:?}", self))?;
            let capture = &mut output.capture;
            if capture.done {
                return Ok(());
            }

            match event {
                Event::Transform { transform } => capture
                    .transform
                    .set(transform)
                    .map_err(|_| eyre!("Capture frame reconfigured for {self:?} {output:?}"))?,
                Event::Damage { x, y, width, height } => {}
                Event::Ready => {
                    if capture.done {
                        bail!("Got second ready for {self:?}, {output:?})");
                    }
                    capture.done = true;
                    let start = Instant::now();
                    let image = capture.buffer.get().unwrap().read()?;
                    println!("{:?}", start.elapsed());

                    image.save(format!("/tmp/screenshotter/{}.pnm", self.0))?;
                    println!("{:?}", start.elapsed());
                    capture.session.take().unwrap().destroy();
                    capture.frame.take().unwrap().destroy();
                }
                Event::Failed { reason } => bail!("Screenshot failed for {self:?} {reason:?}"),
                Event::PresentationTime { .. } | _ => {}
            }
            Ok(())
        });
    }
}

// Probably all unnecessary
impl Drop for Selection {
    fn drop(&mut self) {
        if let Some(layer_surface) = self.layer_surface.take() {
            layer_surface.destroy();
        }
        if let Some(fract_scale) = self.fract_scale.take() {
            fract_scale.destroy();
        }
        if let Some(layer_surface) = self.layer_surface.take() {
            layer_surface.destroy();
        }
        if let Some(viewport) = self.viewport.take() {
            viewport.destroy();
        }
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        if let Some(session) = self.session.take() {
            session.destroy();
        }
        if let Some(frame) = self.frame.take() {
            frame.destroy();
        }
    }
}

impl Drop for Output {
    fn drop(&mut self) {
        if let Some(surface) = self.surface.take() {
            surface.destroy();
        }
    }
}
