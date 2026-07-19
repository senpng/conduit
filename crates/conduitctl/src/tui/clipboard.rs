//! Copy text to the system clipboard (OS-native tools, no extra crate).

use std::io::Write;
use std::process::{Command, Stdio};

/// Write `text` to the system clipboard.
///
/// Backends tried in order:
/// - macOS: `pbcopy`
/// - Wayland: `wl-copy`
/// - X11: `xclip` then `xsel`
/// - Windows: `clip`
pub fn copy_to_clipboard(text: &str) -> Result<(), String> {
    if text.is_empty() {
        return Err("nothing to copy".into());
    }

    #[cfg(target_os = "macos")]
    {
        return pipe_to("pbcopy", &[], text);
    }

    #[cfg(target_os = "windows")]
    {
        return pipe_to("clip", &[], text);
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        // Prefer Wayland when available, then X11 tools.
        if std::env::var_os("WAYLAND_DISPLAY").is_some() {
            if pipe_to("wl-copy", &[], text).is_ok() {
                return Ok(());
            }
        }
        if pipe_to("xclip", &["-selection", "clipboard"], text).is_ok() {
            return Ok(());
        }
        if pipe_to("xsel", &["--clipboard", "--input"], text).is_ok() {
            return Ok(());
        }
        Err(
            "no clipboard tool found (install wl-copy, xclip, or xsel)".into(),
        )
    }
}

fn pipe_to(cmd: &str, args: &[&str], text: &str) -> Result<(), String> {
    let mut child = Command::new(cmd)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("{cmd}: {e}"))?;
    {
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| format!("{cmd}: no stdin"))?;
        stdin
            .write_all(text.as_bytes())
            .map_err(|e| format!("{cmd} write: {e}"))?;
        // Drop stdin to close pipe so the tool exits.
    }
    let status = child
        .wait()
        .map_err(|e| format!("{cmd} wait: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{cmd} exited with {status}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_rejected() {
        assert!(copy_to_clipboard("").is_err());
    }
}
