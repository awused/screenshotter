use color_eyre::eyre::{bail, eyre};
use wayland_client::protocol::wl_output::{self, WlOutput};
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{Dispatch, NoopIgnore};
use wayland_protocols::ext::image_copy_capture::v1::client::ext_image_copy_capture_manager_v1::Options;
use wayland_protocols::xdg::xdg_output::zv1::client::zxdg_output_v1::{self, ZxdgOutputV1};
use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_shell_v1;
use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_surface_v1::Anchor;

use crate::selection::Region;
use crate::wayland::capture::Capture;
use crate::wayland::overlay::Overlay;
use crate::wayland::{OutputKey, State};


#[derive(Debug)]
pub struct Output {
    wl_output: WlOutput,
    _xdg_output: ZxdgOutputV1,
    region: Region,

    pub capture: Capture,
    pub overlay: Overlay,

    pending_done: usize,

    clean: bool,
    dummy_attempted: bool,
}

impl Output {
    pub fn new(wl_output: WlOutput, _xdg_output: ZxdgOutputV1) -> Self {
        Self {
            wl_output,
            _xdg_output,
            region: Region::default(),

            capture: Capture::default(),
            overlay: Overlay::default(),

            // One for wl_output, one for xdg_output
            pending_done: 2,
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
        qh: &wayland_client::QueueHandle<State>,
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

            if state.select {
                debug!("Selection unimplemented");
                let overlay = &mut output.overlay;
                let compositor = state.protos.compositor();

                let freeze_surface = compositor.create_surface(qh, *self);
                let region = compositor.create_region(qh, NoopIgnore);
                // Zero out the input region for this surface
                freeze_surface.set_input_region(Some(&region));
                region.destroy();

                let fract_scale =
                    state.protos.fractional().get_fractional_scale(&freeze_surface, qh, *self);
                overlay.fract_scale.set(fract_scale).unwrap();

                let viewport =
                    state.protos.viewporter().get_viewport(&freeze_surface, qh, NoopIgnore);
                overlay.freeze_port.set(viewport).unwrap();

                let layer_shell = state.protos.layer_shell();
                let layer_surface = layer_shell.get_layer_surface(
                    &freeze_surface,
                    Some(&output.wl_output),
                    zwlr_layer_shell_v1::Layer::Overlay,
                    "screenshotter-freeze".to_string(),
                    qh,
                    *self,
                );

                layer_surface.set_size(0, 0);
                layer_surface.set_exclusive_zone(-1);
                layer_surface
                    .set_anchor(Anchor::Top | Anchor::Bottom | Anchor::Right | Anchor::Left);

                overlay.layer_surface.set(layer_surface).unwrap();

                freeze_surface.commit();
                overlay.freeze_surface.set(freeze_surface).unwrap();
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

impl Dispatch<WlSurface, State> for OutputKey {
    fn event(
        &self,
        state: &mut State,
        proxy: &WlSurface,
        event: <WlSurface as wayland_client::Proxy>::Event,
        conn: &wayland_client::Connection,
        qhandle: &wayland_client::QueueHandle<State>,
    ) {
        trace!("WlSurface: {self:?} {event:?}");
    }
}
