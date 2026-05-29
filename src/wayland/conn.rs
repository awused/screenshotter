use std::cell::OnceCell;
use std::collections::{BTreeMap, BTreeSet};
use std::io::ErrorKind;
use std::time::Duration;

use color_eyre::Result;
use color_eyre::eyre::{bail, eyre};
use tokio::io::unix::AsyncFd;
use tokio::time::{Instant, timeout_at};
use wayland_client::backend::WaylandError;
use wayland_client::protocol::wl_callback::WlCallback;
use wayland_client::protocol::wl_compositor::WlCompositor;
use wayland_client::protocol::wl_output::WlOutput;
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::protocol::wl_seat::WlSeat;
use wayland_client::protocol::wl_shm::WlShm;
use wayland_client::protocol::wl_subcompositor::WlSubcompositor;
use wayland_client::{Connection, Dispatch, DispatchError, EventQueue, NoopIgnore, Proxy};
use wayland_protocols::ext::image_capture_source::v1::client::ext_output_image_capture_source_manager_v1::ExtOutputImageCaptureSourceManagerV1;
use wayland_protocols::ext::image_copy_capture::v1::client::ext_image_copy_capture_manager_v1::ExtImageCopyCaptureManagerV1;
use wayland_protocols::wp::fractional_scale::v1::client::wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1;
use wayland_protocols::wp::viewporter::client::wp_viewporter::WpViewporter;
use wayland_protocols::xdg::xdg_output::zv1::client::zxdg_output_manager_v1::ZxdgOutputManagerV1;
use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_shell_v1::ZwlrLayerShellV1;

use crate::config::CONFIG;
use crate::ipc::Window;
use crate::wayland::output::Output;
use crate::wayland::protos::Protos;
use crate::wayland::{Global, MouseState, OutputKey, SelectMode, State, Status, magnifier};

pub struct Conn {
    queue: EventQueue<State>,
    _registry: WlRegistry,
    state: State,
    deadline: Option<Instant>,
}

impl Conn {
    #[instrument(level = "error", skip_all)]
    pub fn init(screenshot: bool, select: SelectMode) -> Result<Self> {
        assert!(screenshot || select.sel());
        let con = Connection::connect_to_env()?;
        let display = con.display();

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
                screenshot,
                select,
                status: Status::Initializing,

                formats: BTreeSet::default(),
                outputs: BTreeMap::new(),

                magnifier_crosshairs: OnceCell::default(),

                protos: Protos::default(),

                mouse: MouseState::default(),
                keystate: None,

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

    pub async fn select(&mut self, windows: Vec<Window>) -> Result<()> {
        // Can only select if we've been preparing for it
        assert!(self.state.select.sel());
        if self.state.select == SelectMode::Window && windows.is_empty() {
            bail!("No windows available");
        }

        self.state.windows = windows;

        while self.state.status != Status::Selecting {
            self.poll_once().await?;
        }

        if self.state.outputs.is_empty() {
            bail!("No monitors detected");
        }

        while self.state.status != Status::Done {
            self.poll_once().await?;
        }

        error!("TODO -- handle selection");
        Ok(())
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
        qh: &wayland_client::QueueHandle<State>,
    ) {
        use wl_registry::Event;
        trace!("WlRegistry {event:?}");

        match event {
            Event::Global { name, interface, .. } => {
                if interface == WlOutput::interface().name {
                    if state.status != Status::Initializing {
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
        qhandle: &wayland_client::QueueHandle<State>,
    ) {
        debug!("Finished syncing global state");
        if state.status == Status::Initializing {
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
        // state.format.get().unwrap();

        state.try_handle(|state| {
            if state.select == SelectMode::Region && state.screenshot {
                let buffer = magnifier::draw_crosshair(state, qhandle)?;
                state.magnifier_crosshairs.set(buffer).unwrap();
            }

            Ok(())
        });
    }
}
