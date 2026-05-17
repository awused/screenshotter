use std::collections::{HashSet, VecDeque};

use color_eyre::Result;
use hyprland::data::{Clients, Monitors};
use hyprland::shared::HyprData;
use swayipc::Connection;

use crate::selection::Region;

#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub enum Window {
    Sway(swayipc::Node),
    Hypr(hyprland::data::Client),
}

impl Window {
    pub const fn pid(&self) -> i32 {
        match self {
            Self::Sway(node) => node.pid.unwrap(),
            Self::Hypr(client) => client.pid,
        }
    }

    pub fn id(&self) -> String {
        match self {
            Self::Sway(node) => node.id.to_string(),
            Self::Hypr(client) => client.address.to_string(),
        }
    }

    pub fn name(&self) -> Option<&str> {
        match self {
            Self::Sway(node) => node.name.as_deref(),
            Self::Hypr(client) => Some(&client.title),
        }
    }

    pub fn class(&self) -> Option<&str> {
        match self {
            Self::Sway(node) => node
                .app_id
                .as_deref()
                .or_else(|| node.window_properties.as_ref().and_then(|p| p.class.as_deref())),
            Self::Hypr(client) => Some(&client.class),
        }
    }

    pub const fn region(&self) -> Region {
        match self {
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
            Self::Hypr(client) => Region {
                x: client.at.0 as _,
                y: client.at.1 as _,
                width: client.size.0 as _,
                height: client.size.1 as _,
            },
        }
    }
}

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


#[instrument(level = "error", skip_all)]
fn hyprland() -> Result<Vec<Window>> {
    let clients = Clients::get()?;
    let monitors = Monitors::get()?;
    let active_workspaces: Vec<_> = monitors.into_iter().map(|m| m.active_workspace.id).collect();
    Ok(clients
        .into_iter()
        .filter(|c| c.mapped && active_workspaces.contains(&c.workspace.id))
        .map(Window::Hypr)
        .collect())
}

// Tries hyprland then tries sway
// Returns windows top to bottom, or at least first to last in terms of what must be matched
#[instrument(level = "error", skip_all)]
pub fn visible_windows() -> Result<Vec<Window>> {
    hyprland().or_else(|_e| sway())
}
