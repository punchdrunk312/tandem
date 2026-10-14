//! "Send with CubeCursor" in the Explorer right-click menu.
//!
//! Registered per user (HKCU, no admin rights) through `reg.exe`, so there is nothing
//! to compile against and nothing to uninstall by hand: switching it off deletes the keys.
//! Explorer starts `CubeCursor.exe --send "<path>"`; the running CubeCursor picks that up
//! through the single-instance hand-over (see `lib.rs`).

#[cfg(windows)]
mod imp {
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    /// One entry for files, one for folders.
    const KEYS: [&str; 2] = [
        r"HKCU\Software\Classes\*\shell\TandemSend",
        r"HKCU\Software\Classes\Directory\shell\TandemSend",
    ];

    fn reg(args: &[&str]) -> bool {
        Command::new("reg")
            .args(args)
            .creation_flags(CREATE_NO_WINDOW)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    pub fn is_on() -> bool {
        reg(&["query", KEYS[0]]) && reg(&["query", KEYS[1]])
    }

    pub fn set(on: bool) -> Result<(), String> {
        if !on {
            for k in KEYS {
                // Deleting a key that is not there is fine.
                reg(&["delete", k, "/f"]);
            }
            return Ok(());
        }
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let exe = exe.to_string_lossy().into_owned();
        let command = format!("\"{exe}\" --send \"%1\"");
        for k in KEYS {
            let sub = format!(r"{k}\command");
            let ok = reg(&["add", k, "/ve", "/d", "Send with CubeCursor", "/f"])
                && reg(&["add", k, "/v", "Icon", "/d", exe.as_str(), "/f"])
                && reg(&["add", sub.as_str(), "/ve", "/d", command.as_str(), "/f"]);
            if !ok {
                return Err("Windows would not let CubeCursor add the menu entry".into());
            }
        }
        Ok(())
    }
}

#[cfg(not(windows))]
mod imp {
    pub fn is_on() -> bool {
        false
    }
    pub fn set(_on: bool) -> Result<(), String> {
        Ok(())
    }
}

pub use imp::{is_on, set};
