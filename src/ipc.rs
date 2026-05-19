#[cfg(feature = "hyprland")]
use std::collections::HashMap;
use std::collections::VecDeque;

use color_eyre::Result;
use color_eyre::eyre::eyre;
#[cfg(feature = "hyprland")]
use hyprland::{
    data::{Clients, Monitors, Workspaces},
    shared::HyprData,
};
#[cfg(feature = "sway")]
use swayipc::Connection;

use crate::selection::Region;

#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub enum Window {
    #[cfg(feature = "sway")]
    Sway(swayipc::Node),
    #[cfg(feature = "hyprland")]
    Hypr(hyprland::data::Client, Region),
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

    pub const fn region(&self) -> Region {
        match self {
            #[cfg(feature = "sway")]
            Self::Sway(node) => {
                let x = node.rect.x + node.window_rect.x;
                let y = node.rect.y + node.window_rect.y;
                Region {
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
}

#[cfg(feature = "sway")]
#[instrument(level = "error", skip_all)]
fn sway() -> Result<Vec<Window>> {
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


#[cfg(feature = "hyprland")]
#[instrument(level = "error", skip_all)]
fn hyprland() -> Result<Vec<Window>> {
    let clients = Clients::get()?;
    let monitors = Monitors::get()?;
    let workspaces = Workspaces::get()?;
    let workspaces: HashMap<_, _> = workspaces
        .iter()
        .filter_map(|w| monitors.iter().find(|m| m.active_workspace.id == w.id).map(|m| (w, m)))
        .map(|(w, m)| (w.id, (w, m)))
        .collect();

    let mut clients: Vec<_> = clients
        .into_iter()
        .filter(|c| c.visible && c.mapped && !c.hidden)
        .filter_map(|client| {
            let (workspace, monitor) = workspaces.get(&client.workspace.id)?;
            let c_region = Region {
                x: client.at.0 as _,
                y: client.at.1 as _,
                width: client.size.0 as _,
                height: client.size.1 as _,
            };

            // Windows can be completely invisible or partially offscreen
            if workspace.tiled_layout == "scrolling" && !client.floating {
                let m_region = Region {
                    x: monitor.x,
                    y: monitor.y,
                    width: monitor.width as _,
                    height: monitor.height as _,
                };

                // Can this cross multiple monitors?
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

// Tries hyprland then tries sway
// Returns windows top to bottom, or at least first to last in terms of what must be matched
#[instrument(level = "error", skip_all)]
pub fn visible_windows() -> Result<Vec<Window>> {
    let mut err = eyre!("No connection could be made");

    #[cfg(feature = "hyprland")]
    {
        debug!("Attempting to connect to hyprland");
        match hyprland() {
            Ok(w) => return Ok(w),
            Err(e) => {
                use color_eyre::{Section, SectionExt};

                trace!("Failed to connec to hyprland: {e}");
                err = err.section(e.header("hyprland:"));
            }
        }
    }

    #[cfg(feature = "sway")]
    {
        debug!("Attempting to connect to sway");
        match sway() {
            Ok(w) => return Ok(w),
            Err(e) => {
                use color_eyre::{Section, SectionExt};

                trace!("Failed to connec to sway: {e}");
                err = err.section(e.header("sway:"));
            }
        }
    }

    Err(err)
}
