use std::cell::OnceCell;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};

use color_eyre::eyre::bail;
use color_eyre::{Report, Result};
use input_event_codes::{BTN_LEFT, BTN_RIGHT};
use wayland_client::protocol::wl_output::{self};
use wayland_client::protocol::wl_pointer::{ButtonState, WlPointer};
use wayland_client::protocol::wl_shm::{self};
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{NoopIgnore, QueueHandle};
use xkbcommon::xkb::{Keycode, Keysym, MOD_NAME_SHIFT, STATE_MODS_EFFECTIVE, State as XkbState};

use crate::CLICK_TIME_MS;
use crate::img::Screenshot;
use crate::ipc::Window;
use crate::util::{LFRegion, LPoint, LRegion, MLPoint};
use crate::wayland::buffer::Buffer;
use crate::wayland::magnifier::Magnifier;
use crate::wayland::output::Output;
use crate::wayland::protos::Protos;

// Slightly nicer than state.try_handle but doesn't get formatted
// macro_rules! try_handle {
//     ($state:ident, $( $x:tt )*) => {
//         if let Err(e) = (|| {
//             $($x)*
//
//             Ok(())
//         })() {
//             $state.error = Some(e);
//         }
//     };
// }

struct Global;

mod buffer;
mod capture;
pub mod conn;
mod magnifier;
mod output;
mod overlay;
mod protos;
mod select;

#[derive(Debug, Clone, Copy, PartialOrd, PartialEq, Ord, Eq)]
pub struct OutputKey(u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
// Default order is top to bottom
enum Format {
    Argb8888,
    Bgr888,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Transform(wl_output::Transform);


#[derive(Debug)]
struct Hover {
    entered: WlSurface,
    outkey: OutputKey,
    // During a drag we get mouse events relative to the starting monitor.
    corrected: OutputKey,
}

#[derive(Debug, Default)]
struct MouseState {
    // Local within the given surface
    point: MLPoint,
    hover: Option<Hover>,
}

#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)]
enum Status {
    Initializing,
    Waiting,
    Selecting,
    Done(Selected),
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Mode {
    ScreenshotOnly,
    Region(bool),
    PickWindow,
}

impl Mode {
    pub const fn sel(self) -> bool {
        match self {
            Self::ScreenshotOnly => false,
            Self::Region(_) | Self::PickWindow => true,
        }
    }

    pub const fn shot(self) -> bool {
        match self {
            Self::Region(shoot) => shoot,
            Self::ScreenshotOnly => true,
            Self::PickWindow => false,
        }
    }

    pub const fn magnifier(self) -> bool {
        match self {
            Self::Region(_) => true,
            Self::ScreenshotOnly | Self::PickWindow => false,
        }
    }
}


#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)]
pub enum Selected {
    Nothing,
    Region(LFRegion),
    RegionWindow(LFRegion, Window),
    Window(Window),
}

impl Selected {
    pub const fn window(&self) -> Option<&Window> {
        match self {
            Self::Nothing | Self::Region(_) => None,
            Self::RegionWindow(_, window) | Self::Window(window) => Some(window),
        }
    }

    pub fn region(&self) -> Option<LFRegion> {
        match self {
            Self::Nothing => None,
            Self::Region(lfregion) | Self::RegionWindow(lfregion, _) => Some(*lfregion),
            Self::Window(window) => Some(window.region().into()),
        }
    }

    pub fn int_region(&self) -> Option<LRegion> {
        match self {
            Self::Nothing => None,
            Self::Region(lfregion) | Self::RegionWindow(lfregion, _) => Some(lfregion.int_region()),
            Self::Window(window) => Some(window.region()),
        }
    }
}

pub enum SelectState {
    Hovering,
    OverWindow(usize),
    Dragging {
        start_pixel: LFRegion,
        region: LFRegion,
        start_time: u32,
        // For if this is determined to be a normal click
        initial_window: Option<usize>,
    },
}

impl SelectState {
    pub const fn dragging(&self) -> bool {
        matches!(self, Self::Dragging { .. })
    }
}

struct State {
    mode: Mode,
    status: Status,

    // For now we only really support capturing the same format as we overlay
    formats: BTreeSet<Format>,
    outputs: BTreeMap<OutputKey, Output>,

    magnifier_crosshairs: OnceCell<Rc<Buffer>>,

    protos: Rc<Protos>,

    pointer: OnceCell<WlPointer>,
    mouse: MouseState,
    keystate: Option<XkbState>,
    select_state: SelectState,

    windows: Vec<Window>,

    // If some error happened and we need to die
    error: Option<Report>,
}

impl State {
    fn update_status(&mut self) {
        match self.status {
            Status::Selecting | Status::Initializing | Status::Done(_) => {}
            Status::Waiting => {
                let pending_shots =
                    self.mode.shot() && self.outputs.values().any(|o| !o.capture.done);
                let pending_overlays = self.mode.sel()
                    && self.outputs.values().any(|o| o.overlay.get().is_none_or(|o| !o.ready()));

                if !pending_shots && !pending_overlays {
                    if self.mode.sel() {
                        self.status = Status::Selecting
                    } else {
                        self.status = Status::Done(Selected::Nothing)
                    }
                }
            }
        }
    }

    #[instrument(level = "error", skip_all)]
    fn try_handle(&mut self, f: impl FnOnce(&mut Self) -> Result<()>) {
        if let Err(e) = f(self) {
            self.error = Some(e);
        }
    }

    fn transparent_format(&self) -> Format {
        *self.formats.iter().find(|f| f.transparent()).unwrap_or(&Format::Argb8888)
    }

    // Returns Ok(false) if there's pending work and it needs to be called later
    #[instrument(level = "error", skip(self, qh))]
    fn try_finish_overlay(&self, qh: &QueueHandle<Self>, key: OutputKey) -> Result<bool> {
        if !self.mode.sel() {
            return Ok(true);
        }

        let output = &self.outputs[&key];
        let (capture, overlay) = (&output.capture, &output.overlay.get().unwrap());
        if self.mode.shot() && !capture.done {
            return Ok(false);
        }
        let Some(res) = overlay.initialized_res() else {
            return Ok(false);
        };

        let unscaled = *overlay.unscaled.get().unwrap();
        overlay.freeze_port.set_destination(unscaled.0 as _, unscaled.1 as _);
        overlay
            .freeze_surface
            .set_buffer_transform(overlay.transform.freeze_transform());

        if !self.mode.shot() {
            // Always attach a dummy so we can map the freeze layer even if we never render
            // anything
            let dummy = Buffer::new(
                &self.protos,
                qh,
                self.transparent_format(),
                1 as _,
                1 as _,
                NoopIgnore,
            )?;
            overlay.freeze_surface.attach(Some(&dummy.wl_buffer), 0, 0);
            overlay.freeze_surface.commit();
            return Ok(true);
        }

        debug!("Freezing output");

        if res != capture.transformed_res().unwrap() {
            bail!(
                "Got different capture and output resolutions. output: {res:?}, capture {:?}",
                capture.res.get().unwrap()
            );
        }

        let buffer = capture.buffer.get().unwrap();

        overlay.freeze_surface.attach(Some(&buffer.wl_buffer), 0, 0);
        overlay.freeze_surface.commit();

        if self.mode.magnifier() {
            let cross = self.magnifier_crosshairs.get().unwrap();
            let mag = Magnifier::new(
                &self.protos,
                qh,
                key,
                &output.monitor,
                &overlay.freeze_surface,
                buffer,
                cross,
            )?;
            overlay.magnifier.set(mag).unwrap();
        }

        Ok(true)
    }

    fn pointer_enter(&mut self, surface: WlSurface, point: MLPoint) {
        if self.mouse.hover.as_ref().is_some_and(|Hover { entered: s, .. }| *s == surface) {
            warn!("Mouse entered surface {surface:?} it was already in");
        }

        let key = self
            .outputs
            .iter()
            .find(|(_k, v)| v.overlay.get().unwrap().freeze_surface == surface)
            .unwrap()
            .0;

        self.mouse.hover = Some(Hover {
            entered: surface,
            outkey: *key,
            corrected: *key,
        });
        self.mouse.point = point;
    }

    fn pointer_leave(&mut self, surface: WlSurface) {
        trace!("WlPointer leave: {surface:?}");
        let Some(Hover { entered: s, corrected, .. }) = self.mouse.hover.take() else {
            return;
        };

        if s != surface {
            warn!("Mouse left surface {surface:?} it wasn't in");
        }

        self.outputs
            .get_mut(&corrected)
            .unwrap()
            .overlay
            .get_mut()
            .unwrap()
            .hide_magnifier();
    }

    // Don't handle enter/leave/motion immediately since they can be part of the same event
    fn pointer_frame(&mut self, qh: &QueueHandle<Self>) {
        if !matches!(self.status, Status::Selecting) {
            return;
        }

        let Some(Hover {
            outkey: mut target, ref mut corrected, ..
        }) = self.mouse.hover
        else {
            return;
        };

        // Relative to outkey
        let mut point = self.mouse.point;
        let output = self.outputs.get_mut(&target).unwrap();

        if !output.monitor.logical.valid_mouse(point) {
            let global = output.monitor.local_to_global(point);

            // Just iterating is plenty fast
            let Some((k, o)) = self
                .outputs
                .iter()
                .find(|(_k, v)| v.monitor.logical.contains(global))
                .or_else(|| {
                    // Dragging mouse events can be just barely off-screen.
                    // Could snap these into the region, but that actually seems undesirable.
                    self.outputs.iter().find(|(_k, v)| v.monitor.logical.contains_lenient(global))
                })
            else {
                warn!(
                    "Got bad mouse event and could not tie it to a monitor: {:?} on {:?}",
                    self.mouse.point, target
                );
                return;
            };
            target = *k;
            point = o.monitor.global_to_local(global);
        }

        if *corrected != target {
            warn!("Corrected current monitor from {corrected:?} {target:?}");
            self.outputs
                .get_mut(corrected)
                .unwrap()
                .overlay
                .get_mut()
                .unwrap()
                .hide_magnifier();
            *corrected = target;
        }

        if let Err(e) = self
            .outputs
            .get_mut(corrected)
            .unwrap()
            .overlay
            .get_mut()
            .unwrap()
            .move_magnifier(qh, point)
        {
            self.error = Some(e);
        }

        self.update_overlay(qh);
    }

    fn update_overlay(&mut self, qh: &QueueHandle<Self>) {
        match self.select_state {
            SelectState::Hovering => self.update_hover(qh, None),
            SelectState::OverWindow(i) => self.update_hover(qh, Some(i)),
            SelectState::Dragging { start_pixel, ref mut region, .. } => {
                let Some(Hover { outkey, corrected, .. }) = self.mouse.hover else {
                    warn!("Mouse is not on a surface, not updating overlay");
                    return;
                };

                let end_pixel = if corrected == outkey {
                    self.outputs[&outkey].monitor.global_pixel_bounds(self.mouse.point)
                } else {
                    let point = self.outputs[&outkey].monitor.local_to_global(self.mouse.point);
                    let corrected = &self.outputs[&corrected];
                    let point = corrected.monitor.global_to_local(point);
                    corrected.monitor.global_pixel_bounds(point)
                };
                *region = end_pixel.bounding_region(&start_pixel);

                self.outputs.values_mut().for_each(|out| {
                    if let Err(e) = out.draw_region(qh, Some(*region)) {
                        self.error = Some(e);
                    }
                });
            }
        }
    }

    fn update_hover(&mut self, qh: &QueueHandle<Self>, old: Option<usize>) {
        let new = if let Some(Hover { outkey, .. }) = self.mouse.hover {
            let out = &self.outputs[&outkey];
            let point = out.monitor.local_to_global(self.mouse.point);

            self.windows.iter().enumerate().find(|(_i, w)| w.region().contains(point))
        } else {
            None
        };

        let region = match (new, old) {
            (Some((i, _)), Some(j)) if i == j => return,
            (None, None) => return,
            (Some((i, w)), _) => {
                self.select_state = SelectState::OverWindow(i);
                Some(w.region().into())
            }
            (..) => {
                self.select_state = SelectState::Hovering;
                None
            }
        };

        self.outputs.values_mut().for_each(|out| {
            if let Err(e) = out.draw_region(qh, region) {
                self.error = Some(e);
            }
        });
    }

    fn drag_start(&mut self, time: u32) {
        let initial_window = match (&self.select_state, self.mode) {
            (_, Mode::ScreenshotOnly) | (SelectState::Hovering, Mode::PickWindow) => return,
            (SelectState::Dragging { .. }, _) => {
                debug!("Got left button press or enter while dragging. Ignoring.");
                return;
            }
            (SelectState::OverWindow(i), Mode::PickWindow) => {
                debug!("Mouse down or enter on window, selecting");
                self.status = Status::Done(Selected::Window(self.windows.swap_remove(*i)));
                return;
            }
            (SelectState::Hovering, Mode::Region(_)) => None,
            (SelectState::OverWindow(i), Mode::Region(_)) => Some(*i),
        };


        let Some(Hover { outkey, corrected, .. }) = self.mouse.hover else {
            error!("Mouse down or enter outside of surface. This should never happen.");
            return;
        };
        if outkey != corrected {
            error!("Got mouse down or enter while correcting output, this is weird.");
            return;
        }

        let start_pixel = self.outputs[&outkey].monitor.global_pixel_bounds(self.mouse.point);
        debug!("Drag started at {start_pixel:?}");

        self.select_state = SelectState::Dragging {
            start_pixel,
            region: start_pixel,
            start_time: time,
            initial_window,
        }
    }

    fn drag_end(&mut self, qh: &QueueHandle<Self>, time: u32) {
        let SelectState::Dragging { region, start_time, initial_window, .. } = self.select_state
        else {
            return;
        };
        if time.wrapping_sub(start_time) <= CLICK_TIME_MS {
            if let Some(i) = initial_window {
                self.status = Status::Done(Selected::Window(self.windows.swap_remove(i)));
            } else {
                warn!("Click outside of window, ignoring.");
                self.select_state = SelectState::Hovering;
                self.update_overlay(qh);
            }
        } else {
            self.status =
                Status::Done(region.best_window(&mut self.windows).map_or_else(
                    || Selected::Region(region),
                    |w| Selected::RegionWindow(region, w),
                ));
        }
    }

    fn pointer_button(
        &mut self,
        qh: &QueueHandle<Self>,
        time: u32,
        button: u32,
        b_state: ButtonState,
    ) {
        if button == BTN_RIGHT!() {
            if self.select_state.dragging() {
                debug!("Right mouse button, cancelling drag.");
                self.select_state = SelectState::Hovering;
            }
            return;
        } else if button != BTN_LEFT!() {
            return;
        }


        if b_state == ButtonState::Pressed {
            self.drag_start(time);
        } else if b_state == ButtonState::Released {
            if let Some(keystate) = &self.keystate
                && keystate.mod_name_is_active(MOD_NAME_SHIFT, STATE_MODS_EFFECTIVE)
            {
                debug!("Shift held, not ending drag on mouse up");
                return;
            }
            self.drag_end(qh, time);
        }
    }

    fn key_down(&mut self, qh: &QueueHandle<Self>, time: u32, serial: u32, key: u32) -> Result<()> {
        let Some(ref keystate) = self.keystate else {
            warn!("Got key press with no keymap, treating as escape");
            bail!("Cancelled");
        };

        let key = keystate.key_get_one_sym(Keycode::new(key + 8));

        debug!("Keysym down: {key:?}");
        match key {
            Keysym::Escape | Keysym::Q | Keysym::q => {
                bail!("Cancelled");
            }
            Keysym::BackSpace => {
                if self.select_state.dragging() {
                    self.select_state = SelectState::Hovering;
                }

                self.update_hover(qh, None);
            }
            Keysym::P | Keysym::p => {
                // Select region, only if hovering over one
                if let SelectState::OverWindow(i) = self.select_state {
                    info!("Selecting current window");
                    self.status = Status::Done(Selected::Window(self.windows.swap_remove(i)));
                }
            }
            Keysym::Return | Keysym::KP_5 | Keysym::space => {
                if self.select_state.dragging() {
                    self.drag_end(qh, time);
                } else {
                    // Never count this as a click
                    self.drag_start(time.wrapping_sub(CLICK_TIME_MS + 1));
                    self.update_overlay(qh);
                }
            }
            Keysym::Up | Keysym::KP_Up | Keysym::KP_8 => self.warp_pointer(serial, 0, -1),
            Keysym::Down | Keysym::KP_Down | Keysym::KP_2 => self.warp_pointer(serial, 0, 1),
            Keysym::Left | Keysym::KP_Left | Keysym::KP_4 => self.warp_pointer(serial, -1, 0),
            Keysym::Right | Keysym::KP_Right | Keysym::KP_6 => self.warp_pointer(serial, 1, 0),
            Keysym::KP_Home | Keysym::KP_7 => self.warp_pointer(serial, -1, -1),
            Keysym::KP_Page_Up | Keysym::KP_9 => self.warp_pointer(serial, 1, -1),
            Keysym::KP_End | Keysym::KP_1 => self.warp_pointer(serial, -1, 1),
            Keysym::KP_Page_Down | Keysym::KP_3 => self.warp_pointer(serial, 1, 1),
            _ => {}
        }

        Ok(())
    }

    // TODO -- Does not work when dragging outside of the surface
    fn warp_pointer(&self, serial: u32, dx: i32, dy: i32) {
        (|| -> Option<()> {
            let warp = self.protos.pointer_warp.get()?;
            let hover = self.mouse.hover.as_ref()?;
            let pointer = self.pointer.get()?;

            trace!("Warping by {dx} {dy}");
            let point = self.mouse.point;

            let output = &self.outputs[&hover.outkey];
            let global = output.monitor.local_to_global(self.mouse.point);

            let pixel = if hover.outkey == hover.corrected {
                output.monitor.global_pixel_bounds(point)
            } else {
                let true_output = &self.outputs[&hover.corrected];
                let local = true_output.monitor.global_to_local(global);
                true_output.monitor.global_pixel_bounds(local)
            };

            // +0.5 to hit the center of the target pixel, not the corner
            let x = pixel.width.mul_add(0.5 + dx as f64, pixel.x);
            let y = pixel.height.mul_add(0.5 + dy as f64, pixel.y);

            let MLPoint { x, y } = output.monitor.global_to_local(LPoint { x, y });


            warp.warp_pointer(&hover.entered, pointer, x, y, serial);
            None
        })();
    }

    #[instrument(level = "error", skip_all)]
    fn take_screenshot(self) -> Result<Vec<Screenshot>> {
        let Status::Done(selected) = self.status else {
            unreachable!();
        };

        let region = match selected {
            Selected::Nothing => LFRegion {
                x: f64::MIN,
                y: f64::MIN,
                width: f64::INFINITY,
                height: f64::INFINITY,
            },
            Selected::Region(lfregion) | Selected::RegionWindow(lfregion, _) => lfregion,
            Selected::Window(window) => window.region().into(),
        };

        let segments: Vec<_> = self
            .outputs
            .into_values()
            .filter_map(|o| o.monitor.intersect_rounded(&region).map(|(l, r)| (o, l, r)))
            .map(|(o, logical, monitor_region)| {
                let image = o.capture.take_screenshot(&o.monitor, monitor_region);
                Screenshot {
                    image,
                    logical,
                    monitor_region,
                    monitor: o.monitor,
                }
            })
            .collect();

        if segments.is_empty() {
            bail!("Nothing to screenshot, this shouldn't happen.");
        }

        info!("Captured regions on {} monitors", segments.len());
        debug!(
            "Regions: {:?}",
            segments.iter().map(|s| (s.logical, s.image.dimensions())).collect::<Vec<_>>()
        );

        Ok(segments)
    }
}


impl TryFrom<wl_shm::Format> for Format {
    type Error = ();

    fn try_from(value: wl_shm::Format) -> std::prelude::v1::Result<Self, ()> {
        match value {
            wl_shm::Format::Argb8888 => Ok(Self::Argb8888),
            wl_shm::Format::Bgr888 => Ok(Self::Bgr888),
            _ => Err(()),
        }
    }
}

impl Format {
    pub const fn size(self) -> usize {
        match self {
            Self::Argb8888 => 4,
            Self::Bgr888 => 3,
        }
    }

    pub const fn transparent(self) -> bool {
        match self {
            Self::Argb8888 => true,
            Self::Bgr888 => false,
        }
    }

    pub const fn wl_format(self) -> wl_shm::Format {
        match self {
            Self::Argb8888 => wl_shm::Format::Argb8888,
            Self::Bgr888 => wl_shm::Format::Bgr888,
        }
    }
}

static WEIRD_TRANSFORMS: AtomicBool = AtomicBool::new(false);

impl Transform {
    const fn rotate(self) -> bool {
        !self.0.0.is_multiple_of(2)
    }

    fn freeze_transform(self) -> wl_output::Transform {
        if WEIRD_TRANSFORMS.load(Ordering::Relaxed) {
            match self.0.0 {
                1 => wl_output::Transform::_270,
                3 => wl_output::Transform::_90,
                _ => self.0,
            }
        } else {
            self.0
        }
    }

    // (width, height) are physical as displayed to the user.
    // a 1440p monitor with a rotation applied would be (1440, 2560)
    const fn correct(self, (x, y): (i32, i32), (width, height): (i32, i32)) -> (i32, i32) {
        match self.0.0 {
            1 => (y, width - x - 1),
            2 => (width - x - 1, height - y - 1),
            3 => (height - y - 1, x),
            4 => (width - x - 1, y),
            5 => (y, x),
            6 => (x, height - y - 1),
            7 => (height - y - 1, width - x - 1),
            _ => (x, y),
        }
    }

    const fn normal(self) -> bool {
        self.0.0 == 0
    }
}

impl Default for Transform {
    fn default() -> Self {
        Self(wl_output::Transform::Normal)
    }
}
