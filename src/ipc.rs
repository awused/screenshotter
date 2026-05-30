use color_eyre::eyre::eyre;
use color_eyre::{Report, Result, Section, SectionExt};
use futures::future::pending;
use serde_json::Value;
use tokio::{pin, select};
#[cfg(feature = "hyprland")]
use {
    hyprland::data::{Clients, Monitors, Workspaces},
    hyprland::shared::HyprData,
    std::collections::HashMap,
    tokio::try_join,
};
#[cfg(feature = "sway")]
use {std::collections::VecDeque, swayipc::Connection, tokio::task::spawn_blocking};

#[cfg(feature = "hyprland")]
use crate::util::LRegion;


#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub enum Window {
    #[cfg(feature = "sway")]
    Sway(swayipc::Node),
    #[cfg(feature = "hyprland")]
    Hypr(hyprland::data::Client, LRegion),
}

impl Window {
    pub const fn pid(&self) -> i32 {
        match self {
            #[cfg(feature = "sway")]
            Self::Sway(node) => node.pid.unwrap(),
            #[cfg(feature = "hyprland")]
            Self::Hypr(client, _) => client.pid,
        }
    }

    pub fn id(&self) -> String {
        match self {
            #[cfg(feature = "sway")]
            Self::Sway(node) => node.id.to_string(),
            #[cfg(feature = "hyprland")]
            Self::Hypr(client, _) => client.address.to_string(),
        }
    }

    pub fn name(&self) -> Option<&str> {
        match self {
            #[cfg(feature = "sway")]
            Self::Sway(node) => node.name.as_deref(),
            #[cfg(feature = "hyprland")]
            Self::Hypr(client, _) => Some(&client.title),
        }
    }

    pub fn class(&self) -> Option<&str> {
        match self {
            #[cfg(feature = "sway")]
            Self::Sway(node) => node
                .app_id
                .as_deref()
                .or_else(|| node.window_properties.as_ref().and_then(|p| p.class.as_deref())),
            #[cfg(feature = "hyprland")]
            Self::Hypr(client, _) => Some(&client.class),
        }
    }

    pub const fn region(&self) -> LRegion {
        match self {
            #[cfg(feature = "sway")]
            Self::Sway(node) => {
                let x = node.rect.x + node.window_rect.x;
                let y = node.rect.y + node.window_rect.y;
                LRegion {
                    x,
                    y,
                    width: node.window_rect.width,
                    height: node.window_rect.height,
                }
            }
            #[cfg(feature = "hyprland")]
            Self::Hypr(_client, region) => *region,
        }
    }

    pub fn dump(&self) {
        match self {
            #[cfg(feature = "sway")]
            Self::Sway(node) => println!("{}", serde_json::to_string(&node).unwrap()),
            #[cfg(feature = "hyprland")]
            Self::Hypr(client, _region) => println!("{}", serde_json::to_string(&client).unwrap()),
        }
    }

    pub fn to_json(&self) -> Value {
        let mut out = serde_json::Map::new();
        out.insert("visible_region".to_string(), serde_json::to_value(self.region()).unwrap());

        let obj = match self {
            #[cfg(feature = "sway")]
            Self::Sway(node) => serde_json::to_value(node).unwrap(),
            #[cfg(feature = "hyprland")]
            Self::Hypr(client, _) => serde_json::to_value(client).unwrap(),
        };

        out.insert("window".to_string(), obj);

        Value::Object(out)
    }
}


#[cfg(feature = "sway")]
#[instrument(level = "error", skip_all)]
fn sway_blocking() -> Result<Vec<Window>> {
    let mut con = Connection::new()?;
    let root = con.get_tree()?;

    let mut queue: VecDeque<_> = vec![root].into();
    let mut out = Vec::new();

    while let Some(mut node) = queue.pop_front() {
        if !node.floating_nodes.is_empty() {
            // Floating nodes are ordered from bottom to top, we want top first and we want to
            // process all floating nodes first
            for child in node.floating_nodes.drain(..) {
                queue.push_front(child);
            }
            queue.push_back(node);
            continue;
        }

        for child in node.nodes.drain(..) {
            queue.push_back(child);
        }

        if node.pid.is_some() && node.visible.unwrap_or(false) {
            out.push(Window::Sway(node));
        }
    }

    Ok(out)
}

#[cfg(feature = "sway")]
#[instrument(level = "error", skip_all)]
async fn sway() -> Result<Vec<Window>> {
    spawn_blocking(sway_blocking).await?
}


#[cfg(feature = "hyprland")]
#[instrument(level = "error", skip_all)]
async fn hyprland() -> Result<Vec<Window>> {
    let clients = Clients::get_async();
    let monitors = Monitors::get_async();
    let workspaces = Workspaces::get_async();

    let (clients, monitors, workspaces) = try_join!(clients, monitors, workspaces)?;

    let workspaces: HashMap<_, _> = workspaces
        .iter()
        .filter_map(|w| monitors.iter().find(|m| m.active_workspace.id == w.id).map(|m| (w, m)))
        .map(|(w, m)| (w.id, (w, m)))
        .collect();

    let mut clients: Vec<_> = clients
        .into_iter()
        .filter(|c| c.visible && c.mapped && !c.hidden && c.accepts_input)
        .filter_map(|client| {
            let (workspace, monitor) = workspaces.get(&client.workspace.id)?;
            let c_region = LRegion {
                x: client.at.0 as _,
                y: client.at.1 as _,
                width: client.size.0 as _,
                height: client.size.1 as _,
            };

            // Windows can be completely invisible or partially offscreen
            if workspace.tiled_layout == "scrolling" && !client.floating {
                let m_region = LRegion {
                    x: monitor.x,
                    y: monitor.y,

                    width: (monitor.width as f32 / monitor.scale).round() as _,
                    height: (monitor.height as f32 / monitor.scale).round() as _,
                };

                // Can a scrolling workspace cross multiple monitors?
                c_region.intersect(&m_region).map(|r| (client, r))
            } else {
                Some((client, c_region))
            }
        })
        .collect();

    clients.sort_by_key(|(c, _m)| c.floating);
    clients.reverse();
    Ok(clients.into_iter().map(|(c, r)| Window::Hypr(c, r)).collect())
}

// Tries hyprland and sway
// Returns windows top to bottom, or at least first to last in terms of what must be matched
#[instrument(level = "error", skip_all)]
pub async fn visible_windows() -> Result<Vec<Window>> {
    let make_err = || eyre!("No connection could be made");
    let mut err: Option<Report> = None;
    let mut extend_err = |head: &'static str, e: Report| {
        err = Some(err.take().unwrap_or_else(make_err).section(e.header(head)))
    };
    #[allow(unused)]
    let pending = || pending::<Result<Vec<Window>>>();


    #[cfg(feature = "hyprland")]
    let (hyprland, mut try_hyprland) = (hyprland(), true);
    #[cfg(not(feature = "hyprland"))]
    let (hyprland, mut try_hyprland) = (pending(), false);

    #[cfg(feature = "sway")]
    let (sway, mut try_sway) = (sway(), true);
    #[cfg(not(feature = "sway"))]
    let (sway, mut try_sway) = (pending(), false);

    pin!(hyprland, sway);


    loop {
        select! {
            res = &mut hyprland, if try_hyprland => {
                try_hyprland = false;

                match res {
                    Ok(w) => return Ok(w),
                    Err(e) => {
                        trace!("Failed to connect to hyprland: {e}");
                        extend_err("hyprland:", e);
                    },
                }
            },

            res = &mut sway, if try_sway => {
                try_sway = false;

                match res {
                    Ok(w) => return Ok(w),
                    Err(e) => {
                        trace!("Failed to connect to sway: {e}");
                        extend_err("sway:", e);
                    },
                }
            },

            else => {
                return Err(err.take().unwrap_or_else(make_err))
            }
        }
    }
}
