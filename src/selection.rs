use std::io::Write;
use std::process::{Command, Stdio, exit};

use color_eyre::eyre::{OptionExt, eyre};
use color_eyre::{Result, Section, SectionExt};

use crate::config::SLURP;
use crate::ipc::Window;
use crate::util::LRegion;

#[instrument(level = "debug", skip_all)]
pub fn region(windows: &[Window]) -> Result<LRegion> {
    let mut cmd = Command::new(*SLURP);
    cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());

    let mut child = cmd.spawn()?;
    {
        let mut stdin = child.stdin.take().ok_or_eyre("Child missing pipe")?;

        // Depending on how slurp changes this rev() can be removed.
        for w in windows {
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
