use std::cell::OnceCell;

use color_eyre::eyre::{bail, eyre};
use wayland_client::Dispatch;
use wayland_client::protocol::wl_subsurface::WlSubsurface;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_protocols::wp::fractional_scale::v1::client::wp_fractional_scale_v1::{
    self, WpFractionalScaleV1,
};
use wayland_protocols::wp::viewporter::client::wp_viewport::WpViewport;
use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_surface_v1::{
    self, ZwlrLayerSurfaceV1,
};

use crate::wayland::{OutputKey, State};

#[derive(Debug, Default)]
pub struct Overlay {
    pub fract_scale: OnceCell<WpFractionalScaleV1>,

    pub layer_surface: OnceCell<ZwlrLayerSurfaceV1>,

    pub freeze_surface: OnceCell<WlSurface>,
    pub freeze_port: OnceCell<WpViewport>,

    select_surface: OnceCell<WlSubsurface>,

    pub unscaled: OnceCell<(u32, u32)>,
    // All compositors worth caring about implement fractional scale, so only support that.
    scale: OnceCell<u32>,
}

impl Overlay {
    pub fn initialized_res(&self) -> Option<(i32, i32)> {
        let (w, h) = *self.unscaled.get()?;
        let scale = *self.scale.get()?;

        let w = (w as u64 * scale as u64 + 60) / 120;
        let h = (h as u64 * scale as u64 + 60) / 120;
        Some((w as i32, h as i32))
    }
}


impl Dispatch<WpFractionalScaleV1, State> for OutputKey {
    fn event(
        &self,
        state: &mut State,
        _proxy: &WpFractionalScaleV1,
        event: <WpFractionalScaleV1 as wayland_client::Proxy>::Event,
        _conn: &wayland_client::Connection,
        qh: &wayland_client::QueueHandle<State>,
    ) {
        trace!("FractionalScale: {self:?} {event:?}");
        use wp_fractional_scale_v1::Event;

        state.try_handle(|state| {
            let output =
                state.outputs.get_mut(self).ok_or_else(|| eyre!("No output {:?}", self))?;

            let Event::PreferredScale { scale } = event else {
                return Ok(());
            };

            output
                .overlay
                .scale
                .set(scale)
                .map_err(|_| eyre!("Scale reconfigured for {self:?}"))?;

            state.try_freeze(*self, qh)?;

            Ok(())
        });
    }
}

impl Dispatch<ZwlrLayerSurfaceV1, State> for OutputKey {
    fn event(
        &self,
        state: &mut State,
        proxy: &ZwlrLayerSurfaceV1,
        event: <ZwlrLayerSurfaceV1 as wayland_client::Proxy>::Event,
        _conn: &wayland_client::Connection,
        qh: &wayland_client::QueueHandle<State>,
    ) {
        trace!("LayerSurface: {self:?} {event:?}");
        use zwlr_layer_surface_v1::Event;

        state.try_handle(|state| {
            let output = state.outputs.get(self).ok_or_else(|| eyre!("No output {:?}", self))?;

            match event {
                Event::Configure { serial, width, height } => {
                    if let Some((w, h)) = output.overlay.unscaled.get()
                        && (w, h) != (&width, &height)
                    {
                        bail!("Got second configure for {self:?}")
                    }
                    let _ignored = output.overlay.unscaled.set((width, height));

                    proxy.ack_configure(serial);
                    if output.overlay.scale.get().is_none() {
                        warn!("Hyprland did not set scale before configure");
                        let dummy = state.protos.create_buffer(
                            qh,
                            state.default_format(),
                            1 as _,
                            1 as _,
                        )?;
                        let surface = output.overlay.freeze_surface.get().unwrap();
                        surface.attach(Some(&dummy.wl_buffer), 0, 0);
                        surface.commit();
                    }

                    state.try_freeze(*self, qh)?;
                }
                Event::Closed => bail!("Got closed event for layer surface {self:?}"),
                _ => {}
            }

            Ok(())
        });
    }
}

impl Drop for Overlay {
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
        if let Some(viewport) = self.freeze_port.take() {
            viewport.destroy();
        }
    }
}
