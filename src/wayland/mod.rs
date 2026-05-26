use std::collections::BTreeMap;
use std::io::ErrorKind;

use color_eyre::eyre::eyre;
use color_eyre::{Report, Result};
use tokio::io::unix::AsyncFd;
use wayland_client::backend::WaylandError;
use wayland_client::protocol::wl_callback::WlCallback;
use wayland_client::protocol::wl_compositor::WlCompositor;
use wayland_client::protocol::wl_output::WlOutput;
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::protocol::wl_shm::{self, WlShm};
use wayland_client::{Connection, Dispatch, DispatchError, EventQueue, NoopIgnore, Proxy};
use wayland_protocols::ext::image_capture_source::v1::client::ext_output_image_capture_source_manager_v1::ExtOutputImageCaptureSourceManagerV1;
use wayland_protocols::ext::image_copy_capture::v1::client::ext_image_copy_capture_manager_v1::ExtImageCopyCaptureManagerV1;
use wayland_protocols::wp::fractional_scale::v1::client::wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1;
use wayland_protocols::wp::viewporter::client::wp_viewporter::WpViewporter;
use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_shell_v1::ZwlrLayerShellV1;

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

mod output;
mod protos;

#[derive(Debug, Clone, Copy, PartialOrd, PartialEq, Ord, Eq)]
struct OutputKey(u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Format {
    Argb8888,
}


#[derive(Debug)]
struct State {
    // Expect to take a screenshot
    screenshot: bool,
    // Expect to perform selection
    selection: bool,
    // Whether initial sync is done or not
    synced: bool,
    // If some error happened and we need to die
    error: Option<Report>,

    outputs: BTreeMap<OutputKey, Output>,

    protos: Protos,
}

impl State {
    #[instrument(level = "error", skip_all)]
    fn try_handle(&mut self, f: impl FnOnce(&mut Self) -> Result<()>) {
        if let Err(e) = f(self) {
            self.error = Some(e);
        }
    }
}

pub struct Conn {
    queue: EventQueue<State>,
    _registry: WlRegistry,
    state: State,
}


impl Conn {
    #[instrument(level = "error", skip_all)]
    pub fn init(screenshot: bool, selection: bool) -> Result<Self> {
        assert!(screenshot || selection);
        let con = Connection::connect_to_env()?;
        let display = con.display();

        let queue = con.new_event_queue();
        let _registry = display.get_registry(&queue.handle(), Global);

        display.sync(&queue.handle(), Global);

        Ok(Self {
            queue,
            _registry,
            state: State {
                screenshot,
                selection,
                synced: false,
                error: None,
                outputs: BTreeMap::new(),

                protos: Protos::default(),
            },
        })
    }

    #[instrument(level = "error", skip_all)]
    pub async fn poll(&mut self) -> Result<()> {
        loop {
            self.poll_once().await?;
        }
    }

    pub async fn select(&mut self) -> Result<()> {
        // Can only select if we've been preparing for it
        assert!(self.state.selection);

        while !self.state.synced {
            self.poll_once().await?;
        }

        todo!()
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
            if let Err(e) = fd.readable_mut().await {
                println!("Got socket error {e}");
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
        conn: &Connection,
        qh: &wayland_client::QueueHandle<State>,
    ) {
        match event {
            wl_registry::Event::Global { name, interface, .. } => {
                if interface == WlOutput::interface().name {
                    if state.synced {
                        state.error = Some(eyre!("Got new output after initial sync, exiting"));
                        return;
                    }

                    let wl_output = reg.bind::<WlOutput, _, _>(name, 2, qh, OutputKey(name));
                    let output = Output::new(wl_output);
                    state.outputs.insert(OutputKey(name), output);
                } else if interface == WpFractionalScaleManagerV1::interface().name {
                    let fractional_manager =
                        reg.bind::<WpFractionalScaleManagerV1, _, _>(name, 1, qh, NoopIgnore);
                    state.protos.fractional.set(fractional_manager).unwrap();
                } else if interface == WlCompositor::interface().name {
                    let compositor = reg.bind::<WlCompositor, _, _>(name, 6, qh, NoopIgnore);
                    state.protos.compositor.set(compositor).unwrap();
                } else if interface == WpViewporter::interface().name {
                    let viewporter = reg.bind::<WpViewporter, _, _>(name, 1, qh, NoopIgnore);
                    state.protos.viewporter.set(viewporter).unwrap();
                } else if interface == ZwlrLayerShellV1::interface().name {
                    let layer_shell = reg.bind::<ZwlrLayerShellV1, _, _>(name, 1, qh, NoopIgnore);
                    state.protos.layer_shell.set(layer_shell).unwrap();
                } else if interface == WlShm::interface().name {
                    let shm = reg.bind::<WlShm, _, _>(name, 1, qh, NoopIgnore);
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
                }
            }
            wl_registry::Event::GlobalRemove { name } => {
                println!(
                    "Removing {name}, was known output: {}",
                    state.outputs.remove(&OutputKey(name)).is_some()
                );
            }
            _ => {}
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
        _qhandle: &wayland_client::QueueHandle<State>,
    ) {
        debug!("Finished syncing global state");
        state.synced = true;
        // Die if any of these are not initialized
        state.protos.compositor.get().unwrap();
        state.protos.fractional.get().unwrap();
        state.protos.viewporter.get().unwrap();
        state.protos.layer_shell.get().unwrap();
        state.protos.shm.get().unwrap();
        state.protos.output_capture.get().unwrap();
        state.protos.image_copy.get().unwrap();
    }
}


impl TryFrom<wl_shm::Format> for Format {
    type Error = ();

    fn try_from(value: wl_shm::Format) -> std::prelude::v1::Result<Self, ()> {
        match value {
            wl_shm::Format::Argb8888 => Ok(Self::Argb8888),
            _ => Err(()),
        }
    }
}

impl Format {
    const fn size(&self) -> u32 {
        match self {
            Self::Argb8888 => 4,
        }
    }

    const fn wl_format(&self) -> wl_shm::Format {
        match self {
            Self::Argb8888 => wl_shm::Format::Argb8888,
        }
    }
}
