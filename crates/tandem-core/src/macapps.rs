//! Programs on the Mac, for moving them between computers: which program a window
//! belongs to, the page open in a browser, opening and closing programs, and which are
//! open now.
//!
//! Programs travel the way Windows names them ("Discord.exe"), so they move between a
//! Mac and a PC in both directions: a Mac program goes as its name without spaces plus
//! ".exe", and the browsers by their Windows names.

use crate::proto::AppRef;
use objc2::rc::autoreleasepool;
use objc2_app_kit::{NSApplicationActivationPolicy, NSRunningApplication, NSWorkspace};
use std::process::Command;
use std::time::Duration;

/// Browsers: the Mac app, its bundle, and how Windows calls it.
const BROWSERS: &[(&str, &str, &str)] = &[
    ("Google Chrome", "com.google.Chrome", "chrome.exe"),
    ("Microsoft Edge", "com.microsoft.edgemac", "msedge.exe"),
    ("Firefox", "org.mozilla.firefox", "firefox.exe"),
    ("Brave Browser", "com.brave.Browser", "brave.exe"),
    ("Opera", "com.operasoftware.Opera", "opera.exe"),
    ("Vivaldi", "com.vivaldi.Vivaldi", "vivaldi.exe"),
    ("Arc", "company.thebrowser.Browser", "arc.exe"),
    ("Safari", "com.apple.Safari", "safari.exe"),
];

/// Never moved: the Mac's own parts, terminals, and CubeCursor.
const NEVER: &[&str] = &[
    "Finder",
    "Terminal",
    "iTerm2",
    "System Settings",
    "System Preferences",
    "Activity Monitor",
    "CubeCursor",
    "Tandem",
    "loginwindow",
    "Dock",
];

/// What a program's name looks like on the wire: letters, digits and a few others.
fn plain(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_alphanumeric() || matches!(c, ' ' | '.' | '_' | '-' | '(' | ')' | '+'))
        .collect::<String>()
        .trim()
        .to_string()
}

fn as_app(name: &str, bundle: &str) -> Option<AppRef> {
    if NEVER.contains(&name) {
        return None;
    }
    if let Some((_, _, exe)) = BROWSERS.iter().find(|(n, b, _)| *b == bundle || *n == name) {
        let short = exe.trim_end_matches(".exe");
        let mut c = short.chars();
        let pretty: String = c
            .next()
            .map(|f| f.to_uppercase().chain(c).collect())
            .unwrap_or_default();
        return Some(AppRef {
            name: pretty,
            exe: exe.to_string(),
            url: None,
        });
    }
    let n = plain(name);
    if n.is_empty() {
        return None;
    }
    let app = AppRef {
        name: n.clone(),
        exe: format!("{}.exe", n.replace(' ', "")),
        url: None,
    };
    app.is_sane().then_some(app)
}

/// The program that owns process `pid`, as it travels.
pub fn app_of_pid(pid: i32) -> Option<AppRef> {
    if pid as u32 == std::process::id() {
        return None;
    }
    autoreleasepool(|_| {
        let app = NSRunningApplication::runningApplicationWithProcessIdentifier(pid)?;
        let name = app.localizedName()?.to_string();
        let bundle = app
            .bundleIdentifier()
            .map(|b| b.to_string())
            .unwrap_or_default();
        as_app(&name, &bundle)
    })
}

/// The Mac app that a program from the wire means.
fn mac_name(app: &AppRef) -> String {
    let exe = app.exe.to_ascii_lowercase();
    match BROWSERS.iter().find(|(_, _, e)| *e == exe) {
        Some((n, _, _)) => n.to_string(),
        None => app.name.clone(),
    }
}

/// Runs an AppleScript, giving up after a moment (a program that doesn't answer).
fn osascript(script: &str) -> Option<String> {
    let script = script.to_string();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let out = Command::new("osascript").args(["-e", &script]).output();
        let _ = tx.send(out);
    });
    let out = rx.recv_timeout(Duration::from_millis(1500)).ok()?.ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
}

/// The page in a browser's front window. (Firefox can't tell; macOS asks the person
/// once whether CubeCursor may ask the browser.)
pub fn browser_url(app: &AppRef) -> Option<String> {
    let name = mac_name(app);
    let script = match name.as_str() {
        "Safari" => r#"tell application "Safari" to get URL of front document"#.to_string(),
        "Firefox" => return None,
        n => format!(r#"tell application "{n}" to get URL of active tab of front window"#),
    };
    let url = osascript(&script)?;
    (url.starts_with("https://") || url.starts_with("http://")).then_some(url)
}

/// Opens `app` here: its page for a browser, else the program by name.
pub fn launch(app: &AppRef) -> Result<(), String> {
    if !app.is_sane() {
        return Err("That program can't be moved".into());
    }
    if let Some(url) = &app.url {
        return Command::new("open")
            .arg(url)
            .status()
            .ok()
            .filter(|s| s.success())
            .map(|_| ())
            .ok_or_else(|| "Couldn't open the page on this Mac".into());
    }
    let name = mac_name(app);
    let ok = Command::new("open")
        .args(["-a", &name])
        .status()
        .is_ok_and(|s| s.success());
    if ok {
        Ok(())
    } else {
        Err(format!("{} isn't installed on this Mac", app.name))
    }
}

/// Closes `app` here (it opened on the other computer).
pub fn close(app: &AppRef) {
    if !app.is_sane() || app.url.is_some() || !app.may_close() {
        return;
    }
    let name = mac_name(app).replace('"', "");
    let _ = osascript(&format!(r#"quit app "{name}""#));
}

/// The programs open here that could move (not browsers), a handful at most.
pub fn open_apps() -> Vec<AppRef> {
    autoreleasepool(|_| {
        let ws = NSWorkspace::sharedWorkspace();
        let mut out: Vec<AppRef> = Vec::new();
        for app in ws.runningApplications().iter() {
            if app.activationPolicy() != NSApplicationActivationPolicy::Regular || app.isHidden() {
                continue;
            }
            let Some(name) = app.localizedName().map(|n| n.to_string()) else {
                continue;
            };
            let bundle = app
                .bundleIdentifier()
                .map(|b| b.to_string())
                .unwrap_or_default();
            if let Some(a) = as_app(&name, &bundle).filter(|a| !a.is_browser()) {
                if !out.iter().any(|o| o.exe == a.exe) {
                    out.push(a);
                }
            }
            if out.len() >= 6 {
                break;
            }
        }
        out
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mac_programs_travel_with_windows_names() {
        let d = as_app("Discord", "com.hnc.Discord").unwrap();
        assert_eq!(
            (d.name.as_str(), d.exe.as_str()),
            ("Discord", "Discord.exe")
        );
        let c = as_app("Google Chrome", "com.google.Chrome").unwrap();
        assert_eq!(c.exe, "chrome.exe");
        assert!(c.is_browser());
        assert!(as_app("Finder", "com.apple.finder").is_none());
        assert_eq!(mac_name(&c), "Google Chrome");
        assert_eq!(mac_name(&d), "Discord");
    }
}
