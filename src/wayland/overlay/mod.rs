use std::cell::OnceCell;

use color_eyre::Result;
use color_eyre::eyre::{bail, eyre};
use wayland_client::protocol::wl_output::WlOutput;
use wayland_client::protocol::wl_subsurface::WlSubsurface;
use wayland_client::protocol::wl_surface::{self, WlSurface};
use wayland_client::{Dispatch, NoopIgnore, QueueHandle};
use wayland_protocols::wp::fractional_scale::v1::client::wp_fractional_scale_v1::{
    self, WpFractionalScaleV1,
};
use wayland_protocols::wp::viewporter::client::wp_viewport::WpViewport;
use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_shell_v1;
use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_surface_v1::{
    self, Anchor, KeyboardInteractivity, ZwlrLayerSurfaceV1,
};

use crate::util::{MRegion, Monitor};
use crate::wayland::magnifier::Magnifier;
use crate::wayland::overlay::drawing::{Drawing, DrawingKey};
use crate::wayland::protos::{Buffer, Protos};
use crate::wayland::{Format, MouseState, OutputKey, State, Transform};

mod drawing;

#[derive(Debug)]
pub struct Overlay {
    pub output: OutputKey,

    pub fract_scale: WpFractionalScaleV1,

    pub layer_surface: ZwlrLayerSurfaceV1,

    pub freeze_surface: WlSurface,
    pub freeze_port: WpViewport,

    overlay_surface: WlSurface,
    pub overlay_port: WpViewport,
    overlay_subsurface: WlSubsurface,

    transparent_format: Format,
    // Can be large, only gets initialized if needed
    drawings: Vec<Drawing>,

    pub magnifier: Option<Magnifier>,

    pub unscaled: OnceCell<(u32, u32)>,
    // All compositors worth caring about implement fractional scale, so only support that.
    scale: OnceCell<u32>,

    pub transform: Transform,
}

impl Overlay {
    pub fn ready(&self) -> bool {
        self.unscaled.get().is_some() && self.scale.get().is_some()
    }

    pub fn initialized_res(&self) -> Option<(i32, i32)> {
        let (w, h) = *self.unscaled.get()?;
        let scale = *self.scale.get()?;

        let w = (w as u64 * scale as u64 + 60) / 120;
        let h = (h as u64 * scale as u64 + 60) / 120;
        Some((w as i32, h as i32))
    }

    pub fn hide_magnifier(&self) {
        let Some(ref mag) = self.magnifier else {
            return;
        };

        mag.hide();
        self.freeze_surface.commit();
    }

    pub fn move_magnifier(&self, mouse: &MouseState, monitor: &Monitor) {
        let Some(ref mag) = self.magnifier else {
            return;
        };

        if mag.position(mouse.x, mouse.y, *self.unscaled.get().unwrap(), monitor) {
            self.freeze_surface.commit();
        }
    }

    pub fn show_magnifier(&self, freeze_buffer: &Buffer, crosshair: &Buffer) {
        let Some(ref mag) = self.magnifier else {
            return;
        };

        mag.show(freeze_buffer, crosshair);
    }

    pub fn draw_box(
        &mut self,
        protos: &Protos,
        qhandle: &QueueHandle<State>,
        rect: Option<MRegion>,
    ) -> Result<()> {
        if rect.is_none() && self.drawings.is_empty() {
            return Ok(());
        }
        let (w, h) = self.initialized_res().unwrap();


        let drawing = if let Some(drawing) = self.drawings.iter_mut().find(|d| !d.locked) {
            drawing
        } else {
            let index = self.drawings.len();
            if index > 3 {
                bail!("Too many drawings, they're not being unlocked");
            }

            let buffer = protos.create_buffer(
                qhandle,
                self.transparent_format,
                w,
                h,
                DrawingKey(self.output, index),
            )?;
            self.drawings.push_mut(Drawing { buffer, drawn: None, locked: false })
        };

        if drawing.draw(rect) {
            // Could be smarter here, likely does not matter.
            self.overlay_surface.attach(Some(&drawing.buffer.wl_buffer), 0, 0);
            self.overlay_surface.damage(0, 0, w, h);
            self.overlay_surface.commit();
            self.freeze_surface.commit();
        }

        Ok(())
    }
}

impl Protos {
    #[instrument(level = "error", skip(self, qh))]
    pub fn setup_overlay(
        &self,
        qh: &QueueHandle<State>,
        output: OutputKey,
        wl_out: &WlOutput,
        transparent_format: Format,
        transform: Transform,
        magnifier: bool,
    ) -> Result<Overlay> {
        let compositor = self.compositor();

        let freeze_surface = compositor.create_surface(qh, output);
        let fract_scale = self.fractional().get_fractional_scale(&freeze_surface, qh, output);

        let freeze_port = self.viewporter().get_viewport(&freeze_surface, qh, NoopIgnore);

        let layer_shell = self.layer_shell();
        let layer_surface = layer_shell.get_layer_surface(
            &freeze_surface,
            Some(wl_out),
            zwlr_layer_shell_v1::Layer::Overlay,
            "screenshotter-freeze".to_string(),
            qh,
            output,
        );

        layer_surface.set_size(0, 0);
        layer_surface.set_exclusive_zone(-1);
        layer_surface.set_anchor(Anchor::Top | Anchor::Bottom | Anchor::Right | Anchor::Left);
        layer_surface.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);


        let overlay_surface = compositor.create_surface(qh, NoopIgnore);
        let region = compositor.create_region(qh, NoopIgnore);
        overlay_surface.set_input_region(Some(&region));
        region.destroy();


        let overlay_port = self.viewporter().get_viewport(&overlay_surface, qh, NoopIgnore);
        let overlay_subsurface =
            self.subcompositor()
                .get_subsurface(&overlay_surface, &freeze_surface, qh, NoopIgnore);

        let magnifier =
            if magnifier { Some(self.setup_magnifier(qh, &freeze_surface)?) } else { None };

        freeze_surface.commit();

        Ok(Overlay {
            output,

            fract_scale,
            layer_surface,
            freeze_surface,
            freeze_port,

            overlay_surface,
            overlay_port,
            overlay_subsurface,

            transparent_format,
            drawings: Vec::new(),

            magnifier,
            unscaled: OnceCell::default(),
            scale: OnceCell::default(),
            transform,
        })
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

        let Event::PreferredScale { scale } = event else {
            return;
        };

        state.try_handle(|state| {
            state.outputs[self]
                .overlay
                .get()
                .unwrap()
                .scale
                .set(scale)
                .map_err(|_| eyre!("Scale reconfigured for {self:?}"))?;

            state.try_finish_overlay(qh, *self)?;

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
            let output = &state.outputs[self];

            match event {
                Event::Configure { serial, width, height } => {
                    let overlay = output.overlay.get().unwrap();
                    if let Some((w, h)) = overlay.unscaled.get()
                        && (w, h) != (&width, &height)
                    {
                        bail!("Got second configure for {self:?}")
                    }
                    let _ignored = overlay.unscaled.set((width, height));

                    proxy.ack_configure(serial);

                    if overlay.scale.get().is_none() {
                        let dummy = state.protos.create_buffer(
                            qh,
                            state.transparent_format(),
                            1 as _,
                            1 as _,
                            NoopIgnore,
                        )?;
                        overlay.freeze_surface.attach(Some(&dummy.wl_buffer), 0, 0);
                        overlay.freeze_surface.commit();
                    }

                    state.try_finish_overlay(qh, *self)?;
                }
                Event::Closed => bail!("Got closed event for layer surface {self:?}"),
                _ => {}
            }

            Ok(())
        });
    }
}

impl Dispatch<WlSurface, State> for OutputKey {
    fn event(
        &self,
        state: &mut State,
        _proxy: &WlSurface,
        event: <WlSurface as wayland_client::Proxy>::Event,
        _conn: &wayland_client::Connection,
        qh: &wayland_client::QueueHandle<State>,
    ) {
        use wl_surface::Event;

        let Event::PreferredBufferTransform { transform } = event else {
            return;
        };

        let transform = Transform(transform);

        state.try_handle(|state| {
            let output = &state.outputs[self];

            let overlay = output.overlay.get().unwrap();
            if overlay.transform != transform {
                bail!("Transform changed for output {self:?} to {transform:?}");
            }

            Ok(())
        });
    }
}

impl Drop for Overlay {
    fn drop(&mut self) {
        self.layer_surface.destroy();
        self.fract_scale.destroy();
        self.layer_surface.destroy();

        self.freeze_surface.destroy();
        self.freeze_port.destroy();

        self.overlay_surface.destroy();
        self.overlay_port.destroy();
        self.overlay_subsurface.destroy();
    }
}
