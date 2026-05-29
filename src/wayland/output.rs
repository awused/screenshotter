use std::cell::OnceCell;

use color_eyre::eyre::{bail, eyre};
use wayland_client::protocol::wl_output::{self, WlOutput};
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{Dispatch, NoopIgnore};
use wayland_protocols::ext::image_copy_capture::v1::client::ext_image_copy_capture_manager_v1::Options;
use wayland_protocols::xdg::xdg_output::zv1::client::zxdg_output_v1::{self, ZxdgOutputV1};

use crate::util::{MRegion, Monitor};
use crate::wayland::capture::Capture;
use crate::wayland::overlay::Overlay;
use crate::wayland::{OutputKey, SelectMode, State};


#[derive(Debug)]
pub struct Output {
    pub wl_output: WlOutput,
    _xdg_output: ZxdgOutputV1,
    desc: Monitor,
    pub physical: MRegion,

    pub capture: Capture,
    pub overlay: OnceCell<Overlay>,

    pending_done: usize,
}

impl Output {
    pub fn new(wl_output: WlOutput, _xdg_output: ZxdgOutputV1) -> Self {
        Self {
            wl_output,
            _xdg_output,
            desc: Monitor::default(),
            physical: MRegion::default(),

            capture: Capture::default(),
            overlay: OnceCell::default(),

            // One for wl_output, one for xdg_output
            pending_done: 2,
        }
    }
}

impl Dispatch<WlOutput, State> for OutputKey {
    fn event(
        &self,
        state: &mut State,
        _proxy: &WlOutput,
        event: <WlOutput as wayland_client::Proxy>::Event,
        _conn: &wayland_client::Connection,
        qh: &wayland_client::QueueHandle<State>,
    ) {
        use wl_output::Event;

        trace!("WlOutput: {self:?} {event:?}");

        if let Event::Mode { width, height, .. } = event {
            state.try_handle(|state| {
                let output =
                    state.outputs.get_mut(self).ok_or_else(|| eyre!("No output {self:?}"))?;

                if !output.physical.is_empty() {
                    bail!("Got second mode for output {self:?}");
                }

                output.physical.width = width;
                output.physical.height = height;

                Ok(())
            });
            return;
        }

        // Initialize all the layers/managers for this session
        if !matches!(event, Event::Done) {
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

            if output.desc.region.is_empty() || output.physical.is_empty() {
                bail!("Empty region for output {self:?}");
            }

            // TODO[transform]
            let scale = output.physical.width as f64 / output.desc.region.width as f64;
            if !scale.is_normal() || scale <= 0.01 {
                bail!("Invalid monitor scale {scale} for {self:?}");
            }
            trace!("Calculated monitor scale as {scale}");
            output.desc.scale = scale;

            if state.screenshot {
                // TODO -- allow cursor?
                let source = state.protos.output_capture();
                let source = source.create_source(&output.wl_output, qh, NoopIgnore);
                let session = state.protos.image_copy();
                let session = session.create_session(&source, Options::empty(), qh, *self);
                source.destroy();

                output
                    .capture
                    .session
                    .set(session)
                    .map_err(|_| eyre!("Output {:?} reconfigured", self))?;
            }

            if state.select.sel() {
                output
                    .overlay
                    .set(state.protos.setup_overlay(
                        qh,
                        *self,
                        &output.wl_output,
                        state.screenshot && state.select == SelectMode::Region,
                    )?)
                    .unwrap();
            }

            Ok(())
        });
    }
}

impl Dispatch<ZxdgOutputV1, State> for OutputKey {
    fn event(
        &self,
        state: &mut State,
        _proxy: &ZxdgOutputV1,
        event: <ZxdgOutputV1 as wayland_client::Proxy>::Event,
        _conn: &wayland_client::Connection,
        _qhandle: &wayland_client::QueueHandle<State>,
    ) {
        trace!("XdgOutput: {self:?} {event:?}");
        use zxdg_output_v1::Event;

        state.try_handle(|state| {
            let output =
                state.outputs.get_mut(self).ok_or_else(|| eyre!("No output {:?}", self))?;
            let region = &mut output.desc.region;

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

impl Dispatch<WlSurface, State> for OutputKey {
    fn event(
        &self,
        _state: &mut State,
        _proxy: &WlSurface,
        event: <WlSurface as wayland_client::Proxy>::Event,
        _conn: &wayland_client::Connection,
        _qhandle: &wayland_client::QueueHandle<State>,
    ) {
        // TODO -- might need to limit this to specific surfaces
    }
}
