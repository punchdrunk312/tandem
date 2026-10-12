//! Running CubeCursor as administrator.
//!
//! Windows blocks a normal program from sending mouse and keyboard input into windows
//! that run as administrator (installers, Task Manager, "Run as administrator" apps).
//! That is the reason the remote computer sometimes ignores you. An elevated CubeCursor is
//! allowed to type into them.
//!
//! Two pieces, both optional and off by default:
//! * "Restart as administrator": one UAC question, then the new copy takes over.
//! * "Always start as administrator": a logon task that starts CubeCursor elevated without a
//!   prompt. (A plain `Run` registry entry can't start an elevated program silently.)
//!
//! The catch of elevation: Windows also stops non-elevated programs from dragging files
//! onto an elevated window, so dragging from Explorer and the right-click menu don't work
//! while CubeCursor is elevated. The settings page says so.
//!
//! The protected UAC screen itself (the dimmed "Do you want to allow…" prompt) can never be
//! driven by a normal program; CubeCursor notices it and hands control back instead.

#[cfg(windows)]
mod imp {
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::Security::{
        GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY,
    };
    use windows::Win32::System::Threading::{
        GetCurrentProcess, OpenProcess, OpenProcessToken, WaitForSingleObject, PROCESS_SYNCHRONIZE,
    };

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const TASK: &str = "TandemElevated";
    const FLAG: &str = "--elevated-from=";

    pub fn is_elevated() -> bool {
        unsafe {
            let mut token = HANDLE::default();
            if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
                return false;
            }
            let mut info = TOKEN_ELEVATION::default();
            let mut len = 0u32;
            let ok = GetTokenInformation(
                token,
                TokenElevation,
                Some(&mut info as *mut TOKEN_ELEVATION as *mut _),
                std::mem::size_of::<TOKEN_ELEVATION>() as u32,
                &mut len,
            )
            .is_ok();
            let _ = CloseHandle(token);
            ok && info.TokenIsElevated != 0
        }
    }

    fn quiet(cmd: &mut Command) -> &mut Command {
        cmd.creation_flags(CREATE_NO_WINDOW)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
    }

    /// Starts a second, elevated copy of this program (Windows asks the user once) that
    /// waits for this one to quit. Returns `Ok` only after the user said yes.
    pub fn spawn_elevated_copy() -> Result<(), String> {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let exe = exe.to_string_lossy().replace('\'', "''");
        let script = format!(
            "try {{ Start-Process -FilePath '{exe}' -ArgumentList '{FLAG}{}' -Verb RunAs -ErrorAction Stop; exit 0 }} catch {{ exit 1 }}",
            std::process::id()
        );
        let ok = quiet(Command::new("powershell").args([
            "-NoProfile",
            "-NonInteractive",
            "-WindowStyle",
            "Hidden",
            "-Command",
            script.as_str(),
        ]))
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
        if ok {
            Ok(())
        } else {
            Err(
                "Windows didn't allow it — you need to answer Yes to the administrator question."
                    .into(),
            )
        }
    }

    /// An elevated copy started by `spawn_elevated_copy` waits here until the copy that
    /// launched it has quit, so the single-instance check doesn't turn it away.
    pub fn wait_for_parent() {
        let pid =
            std::env::args().find_map(|a| a.strip_prefix(FLAG).and_then(|v| v.parse::<u32>().ok()));
        let Some(pid) = pid else { return };
        unsafe {
            if let Ok(h) = OpenProcess(PROCESS_SYNCHRONIZE, false, pid) {
                let _ = WaitForSingleObject(h, 20_000);
                let _ = CloseHandle(h);
            }
        }
    }

    pub fn autostart_is_on() -> bool {
        quiet(Command::new("schtasks").args(["/Query", "/TN", TASK]))
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    fn xml_escape(s: &str) -> String {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
    }

    /// Creates or deletes the logon task. Creating one needs an elevated CubeCursor.
    ///
    /// The task is written as XML because `schtasks /Create` flags can't switch off the
    /// defaults that would kill CubeCursor after three days or on battery power.
    pub fn autostart_set(on: bool) -> Result<(), String> {
        if !on {
            quiet(Command::new("schtasks").args(["/Delete", "/TN", TASK, "/F"]))
                .status()
                .ok();
            return Ok(());
        }
        if !is_elevated() {
            return Err("Restart CubeCursor as administrator first.".into());
        }
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let user = format!(
            "{}\\{}",
            std::env::var("USERDOMAIN").unwrap_or_default(),
            std::env::var("USERNAME").unwrap_or_default()
        );
        let xml = format!(
            r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <Triggers>
    <LogonTrigger>
      <Enabled>true</Enabled>
      <UserId>{user}</UserId>
    </LogonTrigger>
  </Triggers>
  <Principals>
    <Principal id="Author">
      <UserId>{user}</UserId>
      <LogonType>InteractiveToken</LogonType>
      <RunLevel>HighestAvailable</RunLevel>
    </Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <AllowHardTerminate>true</AllowHardTerminate>
    <StartWhenAvailable>true</StartWhenAvailable>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <Enabled>true</Enabled>
  </Settings>
  <Actions Context="Author">
    <Exec>
      <Command>{cmd}</Command>
      <Arguments>--hidden</Arguments>
    </Exec>
  </Actions>
</Task>
"#,
            user = xml_escape(&user),
            cmd = xml_escape(&exe.to_string_lossy()),
        );
        // schtasks wants UTF-16 with a byte-order mark.
        let mut bytes: Vec<u8> = vec![0xFF, 0xFE];
        for u in xml.encode_utf16() {
            bytes.extend_from_slice(&u.to_le_bytes());
        }
        let path = std::env::temp_dir().join("tandem-task.xml");
        std::fs::write(&path, bytes).map_err(|e| e.to_string())?;
        let ok = quiet(
            Command::new("schtasks")
                .args(["/Create", "/TN", TASK, "/XML"])
                .arg(&path)
                .arg("/F"),
        )
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
        let _ = std::fs::remove_file(&path);
        if ok {
            Ok(())
        } else {
            Err("Windows would not create the start-up task.".into())
        }
    }
}

#[cfg(not(windows))]
mod imp {
    pub fn is_elevated() -> bool {
        false
    }
    pub fn spawn_elevated_copy() -> Result<(), String> {
        Err("Only available on Windows".into())
    }
    pub fn wait_for_parent() {}
    pub fn autostart_is_on() -> bool {
        false
    }
    pub fn autostart_set(_on: bool) -> Result<(), String> {
        Ok(())
    }
}

pub use imp::{autostart_is_on, autostart_set, is_elevated, spawn_elevated_copy, wait_for_parent};
