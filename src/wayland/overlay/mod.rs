use std::cell::OnceCell;
use std::rc::Rc;

use color_eyre::Result;
use color_eyre::eyre::{bail, eyre};
use wayland_client::protocol::wl_callback::WlCallback;
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

use crate::util::{MLPoint, MRegion};
use crate::wayland::buffer::Buffer;
use crate::wayland::magnifier::Magnifier;
use crate::wayland::overlay::highlight::{Highlight, HighlightKey};
use crate::wayland::protos::Protos;
use crate::wayland::{Format, OutputKey, State, Transform};

mod highlight;

#[derive(Debug)]
struct OverlayKey(OutputKey);

#[derive(Debug)]
pub struct Overlay {
    pub output: OutputKey,
    protos: Rc<Protos>,

    pub fract_scale: WpFractionalScaleV1,

    pub layer_surface: ZwlrLayerSurfaceV1,

    pub freeze_surface: WlSurface,
    pub freeze_port: WpViewport,

    overlay_surface: WlSurface,
    pub overlay_port: WpViewport,
    overlay_subsurface: WlSubsurface,

    pending_region: Option<MRegion>,
    overlay_frame: Option<WlCallback>,

    transparent_format: Format,
    // Can be large, only gets initialized if needed
    highlights: Vec<Highlight>,
    last_highlight: (usize, Option<MRegion>),

    pub magnifier: OnceCell<Magnifier>,

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

    pub fn hide_magnifier(&mut self) {
        let Some(mag) = self.magnifier.get_mut() else {
            return;
        };

        mag.hide();
        self.freeze_surface.commit();
    }

    pub fn move_magnifier(&mut self, qh: &QueueHandle<State>, point: MLPoint) -> Result<()> {
        let Some(mag) = self.magnifier.get_mut() else {
            return Ok(());
        };

        if mag.draw(qh, point)? {
            self.freeze_surface.commit();
        }
        Ok(())
    }

    pub fn draw_box(&mut self, qhandle: &QueueHandle<State>, rect: Option<MRegion>) -> Result<()> {
        if (rect.is_none() && self.highlights.is_empty()) || self.last_highlight.1 == rect {
            return Ok(());
        }

        self.pending_region = rect;

        if self.overlay_frame.is_some() {
            return Ok(());
        }

        let (w, h) = self.initialized_res().unwrap();

        // Even if it has the same rectangle drawn, we need to find one that's unlocked
        let (index, high) = if let Some(high) = self
            .highlights
            .iter_mut()
            .enumerate()
            .find(|(_, d)| !d.locked && d.drawn == rect)
        {
            high
        } else if let Some(high) = self.highlights.iter_mut().enumerate().find(|(_, d)| !d.locked) {
            high
        } else {
            let index = self.highlights.len();
            if index > 3 {
                bail!("Too many highlight buffers, they're not being unlocked");
            }
            debug!("Creating highlight buffer {index} for {:?}", self.output);

            let buffer = Buffer::new(
                &self.protos,
                qhandle,
                self.transparent_format,
                w,
                h,
                HighlightKey(self.output, index),
            )?;
            (
                index,
                self.highlights.push_mut(Highlight { buffer, drawn: None, locked: false }),
            )
        };

        // TODO - it'd be faster to keep and reuse a single tiny transparent buffer for when
        // nothing is visible
        if high.draw(rect) || index != self.last_highlight.0 {
            // Could be smarter here, likely does not matter.
            self.overlay_surface.attach(Some(&high.buffer.wl_buffer), 0, 0);
            self.overlay_surface.damage(0, 0, w, h);
            // For sway the viewport resets
            let unscaled = self.unscaled.get().unwrap();
            self.overlay_port.set_destination(unscaled.0 as _, unscaled.1 as _);

            self.overlay_frame = Some(self.overlay_surface.frame(qhandle, OverlayKey(self.output)));

            self.overlay_surface.commit();
            self.freeze_surface.commit();
        }
        self.last_highlight = (index, rect);

        Ok(())
    }

    #[instrument(level = "error", skip(protos, qh, wl_out))]
    pub fn new(
        protos: &Rc<Protos>,
        qh: &QueueHandle<State>,
        output: OutputKey,
        wl_out: &WlOutput,
        transparent_format: Format,
        transform: Transform,
        magnifier: bool,
    ) -> Result<Self> {
        let compositor = protos.compositor();

        let freeze_surface = compositor.create_surface(qh, output);
        let fract_scale = protos.fractional().get_fractional_scale(&freeze_surface, qh, output);

        let freeze_port = protos.viewporter().get_viewport(&freeze_surface, qh, NoopIgnore);

        let layer_shell = protos.layer_shell();
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


        let overlay_port = protos.viewporter().get_viewport(&overlay_surface, qh, NoopIgnore);
        let overlay_subsurface = protos.subcompositor().get_subsurface(
            &overlay_surface,
            &freeze_surface,
            qh,
            NoopIgnore,
        );


        freeze_surface.commit();

        Ok(Self {
            output,
            protos: protos.clone(),

            fract_scale,
            layer_surface,
            freeze_surface,
            freeze_port,

            overlay_surface,
            overlay_port,
            overlay_subsurface,

            overlay_frame: None,
            pending_region: None,

            transparent_format,
            highlights: Vec::new(),
            last_highlight: Default::default(),

            magnifier: OnceCell::default(),
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
                    if let Some((w, h)) = overlay.unscaled.get() {
                        if (w, h) != (&width, &height) {
                            bail!("Got second configure for {self:?}")
                        }
                        return Ok(());
                    }
                    let _ignored = overlay.unscaled.set((width, height));

                    proxy.ack_configure(serial);

                    if overlay.scale.get().is_none() {
                        let dummy = Buffer::new(
                            &state.protos,
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
        _qh: &wayland_client::QueueHandle<State>,
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

impl Dispatch<WlCallback, State> for OverlayKey {
    fn event(
        &self,
        state: &mut State,
        _proxy: &WlCallback,
        _event: <WlCallback as wayland_client::Proxy>::Event,
        _conn: &wayland_client::Connection,
        qhandle: &QueueHandle<State>,
    ) {
        state.try_handle(|state| {
            let overlay = state.outputs.get_mut(&self.0).unwrap().overlay.get_mut().unwrap();
            overlay.overlay_frame = None;
            overlay.draw_box(qhandle, overlay.pending_region)
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
