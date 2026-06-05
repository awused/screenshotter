use std::cell::OnceCell;
use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::io::ErrorKind;
use std::rc::Rc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use color_eyre::Result;
use color_eyre::eyre::{bail, eyre};
use tokio::io::unix::AsyncFd;
use tokio::time::{Instant, timeout_at};
use wayland_client::backend::WaylandError;
use wayland_client::protocol::wl_callback::WlCallback;
use wayland_client::protocol::wl_compositor::WlCompositor;
use wayland_client::protocol::wl_keyboard::{self, KeyState, KeymapFormat, WlKeyboard};
use wayland_client::protocol::wl_output::WlOutput;
use wayland_client::protocol::wl_pointer::{self, WlPointer};
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::protocol::wl_seat::{self, Capability, WlSeat};
use wayland_client::protocol::wl_shm::{self, WlShm};
use wayland_client::protocol::wl_subcompositor::WlSubcompositor;
use wayland_client::{Connection, Dispatch, DispatchError, EventQueue, NoopIgnore, Proxy, QueueHandle};
use wayland_protocols::ext::image_capture_source::v1::client::ext_output_image_capture_source_manager_v1::ExtOutputImageCaptureSourceManagerV1;
use wayland_protocols::ext::image_copy_capture::v1::client::ext_image_copy_capture_manager_v1::ExtImageCopyCaptureManagerV1;
use wayland_protocols::wp::cursor_shape::v1::client::wp_cursor_shape_device_v1::Shape;
use wayland_protocols::wp::cursor_shape::v1::client::wp_cursor_shape_manager_v1::WpCursorShapeManagerV1;
use wayland_protocols::wp::fractional_scale::v1::client::wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1;
use wayland_protocols::wp::pointer_warp::v1::client::wp_pointer_warp_v1::WpPointerWarpV1;
use wayland_protocols::wp::viewporter::client::wp_viewporter::WpViewporter;
use wayland_protocols::xdg::xdg_output::zv1::client::zxdg_output_manager_v1::ZxdgOutputManagerV1;
use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_shell_v1::ZwlrLayerShellV1;
use xkbcommon::xkb::ffi::XKB_KEYMAP_FORMAT_TEXT_V1;
use xkbcommon::xkb::{Context, Keymap, State as XkbState};

use crate::config::CONFIG;
use crate::img::Screenshot;
use crate::ipc::Window;
use crate::util::MLPoint;
use crate::wayland::output::Output;
use crate::wayland::{Global, Mode, MouseState, OutputKey, SelectState, Selected, State, Status, WEIRD_TRANSFORMS, magnifier};

pub struct Conn {
    queue: EventQueue<State>,
    _registry: WlRegistry,
    state: State,
    deadline: Option<Instant>,
}

impl Conn {
    #[instrument(level = "error", skip_all)]
    pub fn init(mode: Mode) -> Result<Self> {
        let con = Connection::connect_to_env()?;
        let display = con.display();

        // Yeah this is weird
        if let Ok(name) = env::var("XDG_CURRENT_DESKTOP")
            && name == "Hyprland"
        {
            // Only affects transforms 1 and 3
            debug!("Hyprland detected, setting weird transforms flag");
            WEIRD_TRANSFORMS.store(true, Ordering::Relaxed);
        }

        let queue = con.new_event_queue();
        let _registry = display.get_registry(&queue.handle(), Global);

        display.sync(&queue.handle(), Global);

        let deadline = if CONFIG.timeout > 0 {
            Some(Instant::now() + Duration::from_secs(CONFIG.timeout))
        } else {
            None
        };

        Ok(Self {
            queue,
            _registry,
            state: State {
                mode,
                status: Status::Initializing,

                formats: BTreeSet::default(),
                outputs: BTreeMap::new(),

                magnifier_crosshairs: OnceCell::default(),

                protos: Rc::default(),

                pointer: OnceCell::default(),
                mouse: MouseState::default(),
                keystate: None,
                select_state: SelectState::Hovering,

                windows: Vec::new(),

                error: None,
            },
            deadline,
        })
    }

    #[instrument(level = "error", skip_all)]
    pub async fn poll(&mut self) -> Result<()> {
        loop {
            self.poll_once().await?;
        }
    }

    pub async fn run(&mut self, windows: Vec<Window>) -> Result<&Selected> {
        // Can only select if we've been preparing for it
        if self.state.mode == Mode::PickWindow && windows.is_empty() {
            bail!("No windows available");
        }

        info!("Starting selection {:?} with {} windows", self.state.mode, windows.len());
        self.state.windows = windows;

        while !matches!(self.state.status, Status::Selecting | Status::Done(_)) {
            self.poll_once().await?;
        }

        if self.state.outputs.is_empty() {
            bail!("No monitors detected");
        }

        // Force a full pointer frame now, to make it highlight things if it wasn't.
        if self.state.mode.sel() {
            self.state.pointer_frame(&self.queue.handle());
        }

        // I'm convinced there's a borrow checker bug here
        while !matches!(self.state.status, Status::Done(_)) {
            self.poll_once().await?;
        }

        if let Status::Done(sel) = &self.state.status {
            info!("Selected region {:?}", sel.region());
            return Ok(sel);
        }
        unreachable!();
    }

    fn flush(&self) -> Result<()> {
        if let Err(e) = self.queue.flush()
            && !ignore_wayland(&e)
        {
            return Err(e.into());
        }
        Ok(())
    }

    #[instrument(level = "error", skip_all)]
    async fn poll_once(&mut self) -> Result<()> {
        self.flush()?;

        'outer: {
            let Some(guard) = self.queue.prepare_read() else {
                break 'outer;
            };

            let mut fd = AsyncFd::new(guard.connection_fd())?;

            let read_result = if let Some(deadline) = self.deadline {
                match timeout_at(deadline, fd.readable_mut()).await {
                    Ok(r) => r,
                    Err(_e) => bail!("Timeout exceeded, exiting"),
                }
            } else {
                fd.readable_mut().await
            };

            if let Err(e) = read_result {
                error!("Got socket error {e}");
                if ignore_error(&e) {
                    break 'outer;
                }
                return Err(e.into());
            }


            drop(fd);
            if let Err(e) = guard.read()
                && !ignore_wayland(&e)
            {
                return Err(e.into());
            }
        }

        if let Err(e) = self.queue.dispatch_pending(&mut self.state)
            && !ignore_dispatch(&e)
        {
            return Err(e.into());
        }

        if let Some(e) = self.state.error.take() {
            return Err(e);
        }

        self.state.update_status();

        Ok(())
    }

    pub fn selected_screenshot(self) -> Result<Vec<Screenshot>> {
        self.state.take_screenshot()
    }

    pub fn screenshot_window(mut self, window: Window) -> Result<Vec<Screenshot>> {
        if !matches!(self.state.status, Status::Done(Selected::Nothing)) {
            bail!("Expected nothing to be selected, but was {:?}", self.state.status);
        }
        self.state.status = Status::Done(Selected::Window(window));
        self.state.take_screenshot()
    }
}

fn ignore_dispatch(error: &DispatchError) -> bool {
    if let DispatchError::Backend(e) = error
        && ignore_wayland(e)
    {
        true
    } else {
        false
    }
}

fn ignore_wayland(error: &WaylandError) -> bool {
    if let WaylandError::Io(e) = error
        && ignore_error(e)
    {
        true
    } else {
        false
    }
}

fn ignore_error(error: &std::io::Error) -> bool {
    error.kind() == ErrorKind::WouldBlock || error.kind() == ErrorKind::Interrupted
}

impl Dispatch<WlRegistry, State> for Global {
    fn event(
        &self,
        state: &mut State,
        reg: &WlRegistry,
        event: <WlRegistry as wayland_client::Proxy>::Event,
        _conn: &Connection,
        qh: &QueueHandle<State>,
    ) {
        use wl_registry::Event;
        trace!("WlRegistry {event:?}");

        match event {
            Event::Global { name, interface, .. } => {
                if interface == WlOutput::interface().name {
                    if !matches!(state.status, Status::Initializing) {
                        state.error = Some(eyre!("Got new output after initial sync, exiting"));
                        return;
                    }

                    let wl_output = reg.bind::<WlOutput, _, _>(name, 2, qh, OutputKey(name));
                    // Assume the output protocol is available by now
                    let xdg_output =
                        state.protos.xdg_output().get_xdg_output(&wl_output, qh, OutputKey(name));
                    let output = Output::new(wl_output, xdg_output);
                    state.outputs.insert(OutputKey(name), output);
                } else if interface == WpFractionalScaleManagerV1::interface().name {
                    let fractional_manager =
                        reg.bind::<WpFractionalScaleManagerV1, _, _>(name, 1, qh, NoopIgnore);
                    state.protos.fractional.set(fractional_manager).unwrap();
                } else if interface == WlCompositor::interface().name {
                    let compositor = reg.bind::<WlCompositor, _, _>(name, 6, qh, NoopIgnore);
                    state.protos.compositor.set(compositor).unwrap();
                } else if interface == WlSubcompositor::interface().name {
                    let subcompositor = reg.bind::<WlSubcompositor, _, _>(name, 1, qh, NoopIgnore);
                    state.protos.subcompositor.set(subcompositor).unwrap();
                } else if interface == WpViewporter::interface().name {
                    let viewporter = reg.bind::<WpViewporter, _, _>(name, 1, qh, NoopIgnore);
                    state.protos.viewporter.set(viewporter).unwrap();
                } else if interface == ZwlrLayerShellV1::interface().name {
                    let layer_shell = reg.bind::<ZwlrLayerShellV1, _, _>(name, 4, qh, NoopIgnore);
                    state.protos.layer_shell.set(layer_shell).unwrap();
                } else if interface == WlShm::interface().name {
                    let shm = reg.bind::<WlShm, _, _>(name, 1, qh, Self);
                    state.protos.shm.set(shm).unwrap();
                } else if interface == ExtOutputImageCaptureSourceManagerV1::interface().name {
                    let manager = reg.bind::<ExtOutputImageCaptureSourceManagerV1, _, _>(
                        name, 1, qh, NoopIgnore,
                    );
                    state.protos.output_capture.set(manager).unwrap();
                } else if interface == ExtImageCopyCaptureManagerV1::interface().name {
                    let manager =
                        reg.bind::<ExtImageCopyCaptureManagerV1, _, _>(name, 1, qh, NoopIgnore);
                    state.protos.image_copy.set(manager).unwrap();
                } else if interface == ZxdgOutputManagerV1::interface().name {
                    let xdg_output = reg.bind::<ZxdgOutputManagerV1, _, _>(name, 3, qh, NoopIgnore);
                    state.protos.xdg_output.set(xdg_output).unwrap();
                } else if interface == WlSeat::interface().name {
                    let _seat = reg.bind::<WlSeat, _, _>(name, 9, qh, Self);
                } else if interface == WpCursorShapeManagerV1::interface().name {
                    let shape = reg.bind::<WpCursorShapeManagerV1, _, _>(name, 1, qh, NoopIgnore);
                    state.protos.shape_manager.set(shape).unwrap();
                } else if interface == WpPointerWarpV1::interface().name {
                    let warp = reg.bind::<WpPointerWarpV1, _, _>(name, 1, qh, NoopIgnore);
                    state.protos.pointer_warp.set(warp).unwrap();
                }
            }
            Event::GlobalRemove { name } if state.outputs.remove(&OutputKey(name)).is_some() => {
                state.error = Some(eyre!("Removed known output {name}"));
            }
            Event::GlobalRemove { .. } | _ => {}
        }
    }
}

impl Dispatch<WlCallback, State> for Global {
    fn event(
        &self,
        state: &mut State,
        _proxy: &WlCallback,
        _event: <WlCallback as wayland_client::Proxy>::Event,
        _conn: &Connection,
        qhandle: &QueueHandle<State>,
    ) {
        debug!("Finished syncing global state");
        if matches!(state.status, Status::Initializing) {
            state.status = Status::Waiting;
        }

        // Die if any of these are not initialized
        state.protos.compositor.get().unwrap();
        state.protos.subcompositor.get().unwrap();
        state.protos.fractional.get().unwrap();
        state.protos.viewporter.get().unwrap();
        state.protos.layer_shell.get().unwrap();
        state.protos.shm.get().unwrap();
        state.protos.output_capture.get().unwrap();
        state.protos.image_copy.get().unwrap();
        state.protos.xdg_output.get().unwrap();
        state.protos.shape_manager.get().unwrap();
        // state.format.get().unwrap();

        state.try_handle(|state| {
            if state.mode.magnifier() {
                let buffer = magnifier::draw_crosshair(state, qhandle)?;
                state.magnifier_crosshairs.set(buffer.into()).unwrap();
            }

            Ok(())
        });
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
                state.pointer_enter(surface, MLPoint { x: surface_x, y: surface_y });

                let shape = state.protos.shape_manager().get_pointer(proxy, qhandle, NoopIgnore);
                shape.set_shape(serial, Shape::Crosshair);
                shape.destroy();
            }
            Event::Leave { surface, .. } => {
                state.pointer_leave(surface);
            }
            Event::Motion { surface_x, surface_y, .. } => {
                state.mouse.point.x = surface_x;
                state.mouse.point.y = surface_y;
            }
            Event::Frame => state.pointer_frame(qhandle),
            Event::Button { time, button, state: button_state, .. } => {
                trace!("WlPointer button: {button:?} {button_state:?}");
                state.pointer_button(qhandle, time, button, button_state);
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
        _proxy: &WlKeyboard,
        event: <WlKeyboard as Proxy>::Event,
        _conn: &Connection,
        qh: &QueueHandle<State>,
    ) {
        // debug!("WlKeyboard: {event:?}");
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
                Event::Key { serial, time, key, state: key_state } => {
                    if key_state == KeyState::Pressed || key_state == KeyState::Repeated {
                        state.key_down(qh, time, serial, key)?;
                    }
                }
                Event::Modifiers {
                    serial: _,
                    mods_depressed,
                    mods_latched,
                    mods_locked,
                    group,
                } => {
                    if let Some(keystate) = &mut state.keystate {
                        keystate.update_mask(
                            mods_depressed,
                            mods_latched,
                            mods_locked,
                            0,
                            0,
                            group,
                        );
                    }
                }
                Event::Enter { .. } | Event::Leave { .. } | Event::RepeatInfo { .. } | _ => {}
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
        _conn: &Connection,
        qh: &QueueHandle<State>,
    ) {
        trace!("WlSeat: {event:?}");

        let wl_seat::Event::Capabilities { capabilities } = event else {
            return;
        };


        if capabilities.contains(Capability::Pointer) {
            let pointer = proxy.get_pointer(qh, Self);
            if let Err(e) = state.pointer.set(pointer) {
                warn!("Seat updated, this isn't handled");
                e.release();
            }
        }

        if capabilities.contains(Capability::Keyboard) {
            proxy.get_keyboard(qh, Self);
        }
    }
}
