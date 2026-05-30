use std::cell::OnceCell;
use std::collections::BTreeSet;

use color_eyre::eyre::{bail, eyre};
use wayland_client::{Dispatch, NoopIgnore};
use wayland_protocols::ext::image_copy_capture::v1::client::ext_image_copy_capture_frame_v1::{
    self, ExtImageCopyCaptureFrameV1,
};
use wayland_protocols::ext::image_copy_capture::v1::client::ext_image_copy_capture_session_v1::{
    self, ExtImageCopyCaptureSessionV1,
};

use crate::wayland::protos::Buffer;
use crate::wayland::{Format, OutputKey, State, Transform};

#[derive(Debug, Default)]
pub struct Capture {
    pub session: OnceCell<ExtImageCopyCaptureSessionV1>,
    pub res: OnceCell<(i32, i32)>,
    // Could make this reset to "better" formats for hdr/whatever
    pub format: OnceCell<Format>,
    pub shm_formats: BTreeSet<Format>,
    frame: OnceCell<ExtImageCopyCaptureFrameV1>,

    // TODO -- test transforms or remove
    pub transform: Transform,

    pub buffer: OnceCell<Buffer>,

    pub done: bool,
}

impl Capture {
    pub fn transformed_res(&self) -> Option<(i32, i32)> {
        let res = *self.res.get()?;
        if self.transform.rotate() {
            // 90/270 rotations
            return Some((res.1, res.0));
        }
        Some(res)
    }
}

impl Dispatch<ExtImageCopyCaptureSessionV1, State> for OutputKey {
    fn event(
        &self,
        state: &mut State,
        proxy: &ExtImageCopyCaptureSessionV1,
        event: <ExtImageCopyCaptureSessionV1 as wayland_client::Proxy>::Event,
        _conn: &wayland_client::Connection,
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
                Event::ShmFormat { format } => {
                    if let Ok(format) = format.try_into() {
                        capture.shm_formats.insert(format);
                    }
                }
                Event::Done => {
                    let format = state.formats.intersection(&capture.shm_formats).next();

                    // Argb8888 has to be supported, so nothing else matters.
                    // Argb8888 can just be dumped into the overlay directly.
                    let format = if let Some(format) = format {
                        *format
                    } else {
                        warn!("No format for capture {self:?}, assuming Argb8888 will work");
                        Format::Argb8888
                    };

                    debug!("Chose capture format for {self:?}: {format:?}");
                    capture.format.set(format).unwrap();

                    if capture.res.get().is_none() {
                        bail!("No resolution for {self:?}: {output:?}");
                    }

                    let frame = proxy.create_frame(qhandle, *self);

                    let (width, height) = *capture.res.get().unwrap();

                    let buffer =
                        state.protos.create_buffer(qhandle, format, width, height, NoopIgnore)?;
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
        _proxy: &ExtImageCopyCaptureFrameV1,
        event: <ExtImageCopyCaptureFrameV1 as wayland_client::Proxy>::Event,
        _conn: &wayland_client::Connection,
        qh: &wayland_client::QueueHandle<State>,
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
                // Event::Transform { transform } => capture
                //     .transform
                //     .set(Transform(transform))
                //     .map_err(|_| eyre!("Capture frame reconfigured for {self:?} {output:?}"))?,
                Event::Ready => {
                    if capture.done {
                        bail!("Got second ready for {self:?}, {output:?})");
                    }
                    capture.done = true;
                    // Just got a Ready event, we can read
                    // let image = unsafe { capture.buffer.get().unwrap().read()? };

                    // image.save(format!("/tmp/screenshotter/{}.pnm", self.0))?;
                    capture.session.take().unwrap().destroy();
                    capture.frame.take().unwrap().destroy();

                    state.try_finish_overlay(qh, *self)?;
                }
                Event::Failed { reason } => bail!("Screenshot failed for {self:?} {reason:?}"),
                Event::Damage { .. } | Event::PresentationTime { .. } | _ => {}
            }
            Ok(())
        });
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
