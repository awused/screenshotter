use std::fmt::Display;
use std::io::Write;
use std::process::{Command, Stdio, exit};

use color_eyre::eyre::{OptionExt, bail, eyre};
use color_eyre::{Result, Section, SectionExt};
use swayipc::Node;

use crate::config::{CONFIG, SLURP};
use crate::ipc::Window;

#[derive(Debug, Eq, PartialEq, Clone, Copy)]
pub struct Region {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

#[instrument(level = "debug", skip_all)]
pub fn region(windows: &Vec<Window>) -> Result<Region> {
    let mut cmd = Command::new(*SLURP);
    cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());

    let mut child = cmd.spawn()?;
    {
        let mut stdin = child.stdin.take().ok_or_eyre("Child missing pipe")?;

        // Depending on how slurp changes this rev() can be removed.
        for w in windows.iter().rev() {
            let r = w.region();
            stdin.write_all(&r.to_string().into_bytes())?;
            stdin.write_all(b"\n")?;
        }
        stdin.flush()?;
    }

    let output = child.wait_with_output()?;

    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        if err.trim() == "selection cancelled" {
            println!("selection cancelled");
            exit(1);
        }

        return Err(eyre!("Slurp process exited with error: status {}", output.status)
            .section(String::from_utf8_lossy(&output.stdout).to_string().header("Stdout:"))
            .section(err.to_string().header("Stderr:")));
    }

    let output = String::from_utf8(output.stdout)?;
    let region = output.try_into()?;

    debug!("Got region from slurp: \"{region}\"");

    Ok(region)
}


impl Display for Region {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{},{} {}x{}", self.x, self.y, self.width, self.height)
    }
}

impl TryFrom<String> for Region {
    type Error = color_eyre::Report;

    fn try_from(input: String) -> std::prelude::v1::Result<Self, Self::Error> {
        let input = input.trim();
        let (x, rest) = input.split_once(',').ok_or_eyre(format!("Invalid region {input}"))?;
        let (y, rest) = rest.split_once(' ').ok_or_eyre(format!("Invalid region {input}"))?;
        let (width, height) = rest.split_once('x').ok_or_eyre(format!("Invalid region {input}"))?;

        Ok(Self {
            x: x.parse()?,
            y: y.parse()?,
            width: width.parse()?,
            height: height.parse()?,
        })
    }
}

impl Region {
    const fn contains(&self, x: i32, y: i32) -> bool {
        self.x <= x && self.x + self.width >= x && self.y <= y && self.y + self.height >= y
    }

    pub fn best_window(&self, mut windows: Vec<Window>) -> Option<Window> {
        // Priotitize exact matches, even if the center is somewhere else
        if let Some(exact) = windows.extract_if(.., |w| *self == w.region()).next() {
            return Some(exact);
        }

        let x = self.x + self.width / 2;
        let y = self.y + self.height / 2;
        windows.into_iter().find(|w| w.region().contains(x, y))
    }
}
