use std::cell::OnceCell;
use std::collections::{BTreeMap, BTreeSet};

use color_eyre::eyre::{bail, eyre};
use color_eyre::{Report, Result};
use input_event_codes::{BTN_LEFT, BTN_RIGHT};
use libc::input_event;
use wayland_client::protocol::wl_keyboard::{self, KeyState, KeymapFormat, WlKeyboard};
use wayland_client::protocol::wl_output::{self};
use wayland_client::protocol::wl_pointer::{self, ButtonState, WlPointer};
use wayland_client::protocol::wl_seat::{self, Capability, WlSeat};
use wayland_client::protocol::wl_shm::{self, WlShm};
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{Connection, Dispatch, NoopIgnore, Proxy, QueueHandle};
use wayland_protocols::wp::cursor_shape::v1::client::wp_cursor_shape_device_v1::Shape;
use xkbcommon::xkb::ffi::XKB_KEYMAP_FORMAT_TEXT_V1;
use xkbcommon::xkb::{Context, Keycode, Keymap, Keysym, State as XkbState};

use crate::ipc::Window;
use crate::util::{LRegion, MLPoint, MRegion};
use crate::wayland::output::Output;
use crate::wayland::protos::{Buffer, Protos};

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

impl Transform {
    const fn rotate(self) -> bool {
        !self.0.0.is_multiple_of(2)
    }

    // TODO -- this is inconsistent between hyprland and sway, needs work.
    const fn freeze_transform(self) -> wl_output::Transform {
        match self.0.0 {
            1 => wl_output::Transform::_270,
            3 => wl_output::Transform::_90,
            _ => self.0,
        }
    }
}

impl Default for Transform {
    fn default() -> Self {
        Self(wl_output::Transform::Normal)
    }
}

#[derive(Debug, Default)]
struct MouseState {
    // Local within the given surface
    x: f64,
    y: f64,
    output: Option<OutputKey>,
    surface: Option<WlSurface>,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum Status {
    Initializing,
    Waiting,
    Selecting,
    Done,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum SelectMode {
    Nothing,
    Region,
    Window,
}

#[derive(Debug)]
#[allow(clippy::large_enum_variant)]
pub enum FinalSelection {
    Nothing,
    MonitorRegion(MRegion),
    LogicalRegion(LRegion),
    Window(Window),
}

impl SelectMode {
    pub const fn sel(self) -> bool {
        match self {
            Self::Nothing => false,
            Self::Region | Self::Window => true,
        }
    }
}


pub enum SelectState {
    Hovering,
    OverWindow(usize),
    Dragging {
        start: MLPoint,
        start_out: OutputKey,
        time: u32,
        // For if this is determined to be a normal click
        initial_window: Option<usize>,
    },
}

struct State {
    // Expect to take a screenshot
    screenshot: bool,
    // Expect to perform selection
    select_mode: SelectMode,
    status: Status,
    final_selection: FinalSelection,

    // For now we only really support capturing the same format as we overlay
    formats: BTreeSet<Format>,
    outputs: BTreeMap<OutputKey, Output>,

    magnifier_crosshairs: OnceCell<Buffer>,

    protos: Protos,

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
            Status::Selecting | Status::Initializing | Status::Done => {}
            Status::Waiting => {
                let pending_shots =
                    self.screenshot && self.outputs.values().any(|o| !o.capture.done);
                let pending_overlays = self.select_mode.sel()
                    && self.outputs.values().any(|o| o.overlay.get().is_none_or(|o| !o.ready()));

                if !pending_shots && !pending_overlays {
                    if self.select_mode.sel() {
                        self.status = Status::Selecting
                    } else {
                        self.status = Status::Done
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
        if !self.screenshot && !self.select_mode.sel() {
            return Ok(true);
        }

        let output = &self.outputs[&key];
        let (capture, overlay) = (&output.capture, &output.overlay.get().unwrap());
        if self.screenshot && !capture.done {
            return Ok(false);
        }
        let Some(res) = overlay.initialized_res() else {
            return Ok(false);
        };

        let unscaled = *overlay.unscaled.get().unwrap();
        overlay.freeze_port.set_destination(unscaled.0 as _, unscaled.1 as _);
        overlay.overlay_port.set_destination(unscaled.0 as _, unscaled.1 as _);
        overlay
            .freeze_surface
            .set_buffer_transform(overlay.transform.freeze_transform());

        if !self.screenshot {
            // Always attach a dummy so we can map the freeze layer even if we never render
            // anything
            let dummy = self.protos.create_buffer(
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

        // TODO -- this might be okay if there are transforms to apply
        if res != capture.transformed_res().unwrap() {
            bail!(
                "Got different capture and output resolutions. output: {res:?}, capture {:?}",
                capture.res.get().unwrap()
            );
        }

        let buffer = capture.buffer.get().unwrap();

        overlay.freeze_surface.attach(Some(&buffer.wl_buffer), 0, 0);

        overlay.freeze_surface.commit();
        Ok(true)
    }

    fn pointer_leave(&mut self, surface: WlSurface) {
        trace!("WlPointer leave: {surface:?}");
        if self.mouse.surface.as_ref() != Some(&surface) {
            warn!("Mouse left surface {surface:?} it wasn't in");
        }

        self.mouse.surface = None;
        let Some(out) = self.mouse.output.take() else {
            return;
        };
        self.outputs[&out].overlay.get().unwrap().hide_magnifier();
    }

    fn pointer_button(&mut self, time: u32, button: u32, b_state: ButtonState) {
        if button == BTN_RIGHT!() {
            if let SelectState::Dragging { .. } = self.select_state {
                debug!("Right mouse button cancelling drag.");
                self.select_state = SelectState::Hovering;
            }
            return;
        } else if button != BTN_LEFT!() {
            return;
        }


        if b_state == ButtonState::Pressed {
            match (&self.select_state, self.select_mode) {
                (_, SelectMode::Nothing) | (SelectState::Hovering, SelectMode::Window) => {}
                (SelectState::Dragging { .. }, _) => {
                    debug!("Got left button press while dragging. Ignoring.")
                }
                (SelectState::OverWindow(i), SelectMode::Window) => {
                    debug!("Mouse down on window, selecting");
                    self.status = Status::Done;

                    self.final_selection = FinalSelection::Window(self.windows.swap_remove(*i));
                }
                (_, SelectMode::Region) => {
                    error!("TODO -- start dragging");
                }
            }
        } else if b_state == ButtonState::Released {
        }
        // match self.select_state {
        //     SelectState::Hovering => ,
        //     SelectState::OverWindow(_) => todo!(),
        //     SelectState::Dragging { start, start_out, time, initial_window } => todo!(),
        // }
    }

    // Don't handle enter/leave/motion immediately since they can be part of the same event
    fn pointer_frame(&mut self, qh: &QueueHandle<Self>) {
        if self.status != Status::Selecting {
            return;
        }

        let Some(ref surface) = self.mouse.surface else { return };

        let output = if let Some(outkey) = self.mouse.output {
            &self.outputs[&outkey]
        } else {
            let (key, output) = self
                .outputs
                .iter()
                .find(|(_k, v)| v.overlay.get().unwrap().freeze_surface == *surface)
                .unwrap();
            self.mouse.output = Some(*key);

            if let Some(crosshair) = self.magnifier_crosshairs.get() {
                let freeze_buffer = output.capture.buffer.get().unwrap();
                output.overlay.get().unwrap().show_magnifier(freeze_buffer, crosshair);
            }
            output
        };

        output.overlay.get().unwrap().move_magnifier(&self.mouse, &output.monitor);

        //if !dragging
        // if dragging {
        // } else {
        // }
        //
        self.update_overlay(qh);
    }

    fn update_overlay(&mut self, qh: &QueueHandle<Self>) {
        match self.select_state {
            SelectState::Hovering => self.update_hover(qh, None),
            SelectState::OverWindow(i) => self.update_hover(qh, Some(i)),
            SelectState::Dragging { start, start_out, time, initial_window } => todo!(),
        }
    }

    fn update_hover(&mut self, qh: &QueueHandle<Self>, old: Option<usize>) {
        let new = if let Some(out) = self.mouse.output {
            let out = &self.outputs[&out];
            let point = out.monitor.local_to_global(MLPoint { x: self.mouse.x, y: self.mouse.y });
            self.windows.iter().enumerate().find(|(_i, w)| w.region().contains(point))
        } else {
            None
        };

        let region = match (new, old) {
            (Some((i, _)), Some(j)) if i == j => return,
            (None, None) => return,
            (Some((i, w)), _) => {
                self.select_state = SelectState::OverWindow(i);
                Some(w.region())
            }
            (..) => {
                self.select_state = SelectState::Hovering;
                None
            }
        };

        self.outputs.values_mut().for_each(|out| {
            if let Err(e) = out.draw_region(&self.protos, qh, region) {
                self.error = Some(e);
            }
        });
    }

    fn handle_key(&self, key: u32) -> Result<()> {
        let Some(ref keystate) = self.keystate else {
            warn!("Got key press with no keymap, treating as escape");
            bail!("Cancelled");
        };

        let key = keystate.key_get_one_sym(Keycode::new(key + 8));

        match key {
            Keysym::Escape | Keysym::Q | Keysym::q => {
                bail!("Cancelled");
            }
            _ => Ok(()),
        }
    }
}

impl Dispatch<WlPointer, State> for Global {
    fn event(
        &self,
        state: &mut State,
        proxy: &WlPointer,
        event: <WlPointer as Proxy>::Event,
        _conn: &Connection,
        qhandle: &QueueHandle<State>,
    ) {
        // These ones are just too spammy to bother
        // trace!("WlPointer {event:?}");
        use wl_pointer::Event;

        match event {
            Event::Enter { surface, surface_x, surface_y, serial } => {
                debug!("WlPointer enter: {surface:?} {surface_x} {surface_y}");
                if state.mouse.surface.as_ref() == Some(&surface) {
                    warn!("Mouse entered surface {surface:?} it was already in");
                }
                state.mouse.output = None;
                state.mouse.surface = Some(surface);
                state.mouse.x = surface_x;
                state.mouse.y = surface_y;

                let shape = state.protos.shape_manager().get_pointer(proxy, qhandle, NoopIgnore);
                shape.set_shape(serial, Shape::Crosshair);
                shape.destroy();
            }
            Event::Leave { surface, .. } => {
                state.pointer_leave(surface);
            }
            Event::Motion { surface_x, surface_y, .. } => {
                state.mouse.x = surface_x;
                state.mouse.y = surface_y;
            }
            Event::Frame => state.pointer_frame(qhandle),
            Event::Button { time, button, state: button_state, .. } => {
                trace!("WlPointer button: {button:?} {button_state:?}");
                state.pointer_button(time, button, button_state);
            }
            Event::Axis { .. }
            | Event::AxisSource { .. }
            | Event::AxisStop { .. }
            | Event::AxisDiscrete { .. }
            | Event::AxisValue120 { .. }
            | Event::AxisRelativeDirection { .. }
            | _ => {}
        }
    }
}

impl Dispatch<WlKeyboard, State> for Global {
    fn event(
        &self,
        state: &mut State,
        proxy: &WlKeyboard,
        event: <WlKeyboard as Proxy>::Event,
        conn: &Connection,
        qhandle: &QueueHandle<State>,
    ) {
        debug!("WlKeyboard: {event:?}");
        use wl_keyboard::Event;

        state.try_handle(|state| {
            match event {
                Event::Keymap { format, fd, size } => {
                    let context = Context::new(0);

                    if format == KeymapFormat::XkbV1 {
                        let keymap = unsafe {
                            Keymap::new_from_fd(
                                &context,
                                fd,
                                size as _,
                                XKB_KEYMAP_FORMAT_TEXT_V1,
                                0,
                            )?
                            .ok_or_else(|| eyre!("Could not build keymap"))?
                        };

                        state.keystate = Some(XkbState::new(&keymap));
                    } else if format == KeymapFormat::NoKeymap && state.keystate.is_none() {
                        let keymap = Keymap::new_from_names(&context, "", "", "", "", None, 0)
                            .ok_or_else(|| eyre!("Could not build keymap"))?;

                        state.keystate = Some(XkbState::new(&keymap));
                    }
                }
                Event::Enter { serial, surface, keys } => {}
                Event::Leave { serial, surface } => {}
                Event::Key { serial, time, key, state: key_state } => {
                    if key_state == KeyState::Pressed {
                        state.handle_key(key)?;
                    }
                }
                Event::Modifiers {
                    serial,
                    mods_depressed,
                    mods_latched,
                    mods_locked,
                    group,
                } => {}
                Event::RepeatInfo { rate, delay } => {}
                _ => {}
            }
            Ok(())
        });
    }
}


impl Dispatch<WlShm, State> for Global {
    fn event(
        &self,
        state: &mut State,
        _proxy: &WlShm,
        event: <WlShm as Proxy>::Event,
        _conn: &Connection,
        _qhandle: &QueueHandle<State>,
    ) {
        trace!("WlShm: {event:?}");
        state.try_handle(|state| {
            if let wl_shm::Event::Format { format } = event
                && let Ok(format) = format.try_into()
            {
                state.formats.insert(format);
            }
            Ok(())
        });
    }
}

impl Dispatch<WlSeat, State> for Global {
    fn event(
        &self,
        state: &mut State,
        proxy: &WlSeat,
        event: <WlSeat as Proxy>::Event,
        conn: &Connection,
        qh: &QueueHandle<State>,
    ) {
        trace!("WlSeat: {event:?}");

        let wl_seat::Event::Capabilities { capabilities } = event else {
            return;
        };


        if capabilities.contains(Capability::Pointer) {
            proxy.get_pointer(qh, Self);
        }

        if capabilities.contains(Capability::Keyboard) {
            proxy.get_keyboard(qh, Self);
        }
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
    const fn size(self) -> usize {
        match self {
            Self::Argb8888 => 4,
            Self::Bgr888 => 3,
        }
    }

    const fn transparent(self) -> bool {
        match self {
            Self::Argb8888 => true,
            Self::Bgr888 => false,
        }
    }

    const fn wl_format(self) -> wl_shm::Format {
        match self {
            Self::Argb8888 => wl_shm::Format::Argb8888,
            Self::Bgr888 => wl_shm::Format::Bgr888,
        }
    }
}
