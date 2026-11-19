//! Windows notifications, shown in the bar: who sent it, the title and a line of text.
//!
//! Windows lets apps read the notifications it shows (Settings → Privacy → Notifications).
//! Every two seconds the list is read; anything new goes to the island. Nothing is kept
//! and nothing leaves this computer.

use serde::Serialize;
use std::sync::atomic::{AtomicBool, Ordering};
use tauri::AppHandle;

/// Switched by the person in Settings → Top bar → Notifications.
static ON: AtomicBool = AtomicBool::new(false);

#[derive(Serialize, Clone)]
pub struct Note {
    pub id: u32,
    pub app: String,
    pub title: String,
    pub body: String,
    /// The app's logo as a `data:` URL, or "".
    pub icon: String,
}

#[tauri::command]
pub fn notify_set(on: bool) {
    ON.store(on, Ordering::Relaxed);
}

pub fn start(app: AppHandle) {
    #[cfg(windows)]
    {
        let _ = std::thread::Builder::new()
            .name("cubecursor-notify".into())
            .spawn(move || win::run(app));
    }
    #[cfg(not(windows))]
    let _ = app;
}

#[cfg(windows)]
mod win {
    use super::*;
    use crate::island;
    use std::collections::{HashMap, HashSet};
    use std::time::Duration;
    use tauri::Emitter;
    use windows::Foundation::Size;
    use windows::UI::Notifications::Management::{UserNotificationListener, UserNotificationListenerAccessStatus};
    use windows::UI::Notifications::{KnownNotificationBindings, NotificationKinds, UserNotification};

    pub fn run(app: AppHandle) {
        let mut seen: HashSet<u32> = HashSet::new();
        let mut first = true;
        let mut told_off = false;
        let mut icons: HashMap<String, String> = HashMap::new();
        loop {
            std::thread::sleep(Duration::from_secs(2));
            if !ON.load(Ordering::Relaxed) {
                first = true;
                continue;
            }
            let Ok(l) = UserNotificationListener::Current() else { continue };
            if l.GetAccessStatus().ok() != Some(UserNotificationListenerAccessStatus::Allowed) {
                if !told_off {
                    told_off = true;
                    log::info!("notifications: Windows doesn't let apps read notifications here");
                }
                continue;
            }
            told_off = false;
            let Ok(list) = l.GetNotificationsAsync(NotificationKinds::Toast).and_then(|o| o.get()) else { continue };
            let mut now = HashSet::new();
            for n in list {
                let Ok(id) = n.Id() else { continue };
                now.insert(id);
                // What was already there when we started is old news.
                if first || seen.contains(&id) {
                    continue;
                }
                if let Some(note) = read(&n, id, &mut icons) {
                    let _ = app.emit_to(island::WINDOW_LABEL, "notification", note);
                }
            }
            seen = now;
            first = false;
        }
    }

    fn read(n: &UserNotification, id: u32, icons: &mut HashMap<String, String>) -> Option<Note> {
        let info = n.AppInfo().ok()?.DisplayInfo().ok()?;
        let app = info.DisplayName().ok()?.to_string();
        let binding = n
            .Notification()
            .ok()?
            .Visual()
            .ok()?
            .GetBinding(&KnownNotificationBindings::ToastGeneric().ok()?)
            .ok()?;
        let texts: Vec<String> = binding
            .GetTextElements()
            .ok()?
            .into_iter()
            .filter_map(|t| t.Text().ok().map(|h| h.to_string()))
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        let title = texts.first().cloned().unwrap_or_else(|| app.clone());
        let mut body = texts.iter().skip(1).cloned().collect::<Vec<_>>().join(" · ");
        if body.chars().count() > 160 {
            body = body.chars().take(157).collect::<String>() + "…";
        }
        let icon = icons
            .entry(app.clone())
            .or_insert_with(|| logo(&info).unwrap_or_default())
            .clone();
        Some(Note { id, app, title, body, icon })
    }

    /// The app's logo, small, as a data URL.
    fn logo(info: &windows::ApplicationModel::AppDisplayInfo) -> Option<String> {
        use base64::Engine as _;
        use windows::Storage::Streams::DataReader;
        let stream = info.GetLogo(Size { Width: 64.0, Height: 64.0 }).ok()?.OpenReadAsync().ok()?.get().ok()?;
        let size = stream.Size().ok()?;
        if size == 0 || size > 1_000_000 {
            return None;
        }
        let reader = DataReader::CreateDataReader(&stream.GetInputStreamAt(0).ok()?).ok()?;
        reader.LoadAsync(size as u32).ok()?.get().ok()?;
        let mut buf = vec![0u8; size as usize];
        reader.ReadBytes(&mut buf).ok()?;
        Some(format!("data:image/png;base64,{}", base64::engine::general_purpose::STANDARD.encode(&buf)))
    }
}
