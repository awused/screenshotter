use std::io::ErrorKind;

use color_eyre::{Report, Result};
use tokio::io::unix::AsyncFd;
use wayland_client::backend::WaylandError;
use wayland_client::protocol::wl_callback::WlCallback;
use wayland_client::protocol::wl_output::WlOutput;
use wayland_client::protocol::wl_registry::WlRegistry;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{Connection, Dispatch, DispatchError, EventQueue};
use wayland_protocols::wp::fractional_scale::v1::client::wp_fractional_scale_v1::WpFractionalScaleV1;
use wayland_protocols::wp::viewporter::client::wp_viewport::WpViewport;
use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_surface_v1::ZwlrLayerSurfaceV1;

struct Global;

#[derive(Debug)]
struct Output {
    wl_output: WlOutput,
    fract_scale: Option<WpFractionalScaleV1>,
    surface: Option<WlSurface>,
    viewport: Option<WpViewport>,
    layer_surface: Option<ZwlrLayerSurfaceV1>,
    // Resolution in logical pixels
    res: Option<(u32, u32)>,
    fractional_scale: Option<u32>,
    int_scale: i32,
    clean: bool,
    dummy_attempted: bool,
}

#[derive(Debug, Default)]
struct State {
    // Expect to take a screenshot
    screenshot: bool,
    // Expect to perform selection
    selection: bool,
    // Whether initial sync is done or not
    synced: bool,
    // If some error happened and we need to die
    error: Option<Report>,
}

pub struct Conn {
    queue: EventQueue<State>,
    _registry: WlRegistry,
    state: State,
}


impl Conn {
    #[instrument(level = "error", skip_all)]
    pub fn init(screenshot: bool, selection: bool) -> Result<Self> {
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
        proxy: &WlRegistry,
        event: <WlRegistry as wayland_client::Proxy>::Event,
        conn: &Connection,
        qhandle: &wayland_client::QueueHandle<State>,
    ) {
        println!("{event:?}");
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
    }
}
