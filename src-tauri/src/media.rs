//! What is playing (Spotify, a browser, any app that talks to Windows' media controls), and
//! the play/pause/next/previous buttons for it. Polled once a second on its own thread and
//! sent to the island as "media"; nothing is sent while nothing changes.
//!
//! Lyrics are optional (off unless the person turns them on in the bar): the song's title,
//! artist, album and length go to lrclib.net (timed lyrics), or, when it has none or can't
//! be reached, the title and artist to lyrics.ovh (plain lyrics). Nothing else is sent.

use serde::Serialize;
use tauri::{AppHandle, Emitter};

use crate::island;

#[derive(Serialize, Clone, PartialEq, Debug, Default)]
pub struct NowPlaying {
    /// Which app is playing ("Spotify.exe", "Chrome"...), as Windows names it.
    pub app: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub playing: bool,
    /// Position and length in milliseconds (0 when the app doesn't say).
    pub position_ms: u64,
    pub duration_ms: u64,
    pub can_next: bool,
    pub can_prev: bool,
    /// The album art as a `data:` URL (the bar takes its colour from it), or "".
    pub art: String,
}

/// Starts the poll (Windows only; elsewhere the island simply never hears "media").
pub fn start(app: AppHandle) {
    #[cfg(windows)]
    {
        let _ = std::thread::Builder::new()
            .name("cubecursor-media".into())
            .spawn(move || sys::run(app));
    }
    #[cfg(not(windows))]
    let _ = app;
}

/// "play_pause", "next" or "prev" for the app that is playing.
#[tauri::command]
pub fn media_control(action: String) -> Result<(), String> {
    #[cfg(windows)]
    return sys::control(&action);
    #[cfg(not(windows))]
    {
        let _ = action;
        Err("Media controls need Windows.".into())
    }
}

/// Time-stamped lyrics ("[mm:ss.xx] line" per line), or plain lyrics, or None.
/// Runs off the UI thread: Tauri runs async commands on its own pool.
#[tauri::command]
pub async fn media_lyrics(title: String, artist: String, album: String, duration_ms: u64) -> Result<Option<Lyrics>, String> {
    tauri::async_runtime::spawn_blocking(move || lyrics(&title, &artist, &album, duration_ms))
        .await
        .map_err(|e| e.to_string())?
}

#[derive(Serialize)]
pub struct Lyrics {
    pub synced: bool,
    pub text: String,
}

fn lyrics(title: &str, artist: &str, album: &str, duration_ms: u64) -> Result<Option<Lyrics>, String> {
    if title.trim().is_empty() {
        return Ok(None);
    }
    let timed = lrclib(title, artist, album, duration_ms);
    if let Ok(Some(l)) = timed {
        return Ok(Some(l));
    }
    match lyrics_ovh(title, artist) {
        Ok(Some(l)) => Ok(Some(l)),
        // Neither had it; report lrclib's problem if it had one.
        Ok(None) => timed.map(|_| None),
        Err(e) => timed.and(Err(e)),
    }
}

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout(std::time::Duration::from_secs(8))
        .user_agent(concat!("CubeCursor/", env!("CARGO_PKG_VERSION"), " (https://cubecursor.shop)"))
        .build()
}

/// Plain lyrics from lyrics.ovh (no timing).
fn lyrics_ovh(title: &str, artist: &str) -> Result<Option<Lyrics>, String> {
    if artist.trim().is_empty() {
        return Ok(None);
    }
    let enc = |s: &str| s.trim().replace('/', " ").replace('?', " ");
    let url = format!(
        "https://api.lyrics.ovh/v1/{}/{}",
        urlencode(&enc(artist)),
        urlencode(&enc(title))
    );
    let body: serde_json::Value = match agent().get(&url).call() {
        Ok(r) => r.into_json().map_err(|e| e.to_string())?,
        Err(ureq::Error::Status(404, _)) => return Ok(None),
        Err(e) => return Err(format!("Could not reach the lyrics library: {e}")),
    };
    Ok(body
        .get("lyrics")
        .and_then(|v| v.as_str())
        .map(|t| t.replace("\r\n", "\n").trim().to_string())
        .filter(|t| !t.is_empty())
        .map(|text| Lyrics { synced: false, text }))
}

fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// Timed lyrics from lrclib.net.
fn lrclib(title: &str, artist: &str, album: &str, duration_ms: u64) -> Result<Option<Lyrics>, String> {
    let agent = ureq::AgentBuilder::new()
        .timeout(std::time::Duration::from_secs(8))
        .user_agent(concat!("CubeCursor/", env!("CARGO_PKG_VERSION"), " (https://cubecursor.shop)"))
        .build();
    let mut req = agent
        .get("https://lrclib.net/api/get")
        .query("track_name", title)
        .query("artist_name", artist);
    if !album.is_empty() {
        req = req.query("album_name", album);
    }
    if duration_ms > 0 {
        req = req.query("duration", &(duration_ms / 1000).to_string());
    }
    let body: serde_json::Value = match req.call() {
        Ok(r) => r.into_json().map_err(|e| e.to_string())?,
        Err(ureq::Error::Status(404, _)) => return Ok(None),
        Err(e) => return Err(format!("Could not reach the lyrics library: {e}")),
    };
    let pick = |k: &str| body.get(k).and_then(|v| v.as_str()).filter(|s| !s.trim().is_empty());
    Ok(pick("syncedLyrics")
        .map(|t| Lyrics { synced: true, text: t.to_string() })
        .or_else(|| pick("plainLyrics").map(|t| Lyrics { synced: false, text: t.to_string() })))
}

#[cfg(windows)]
mod sys {
    use super::*;
    use std::time::Duration;
    use windows::Media::Control::{
        GlobalSystemMediaTransportControlsSession as Session,
        GlobalSystemMediaTransportControlsSessionManager as Manager,
        GlobalSystemMediaTransportControlsSessionPlaybackStatus as Status,
    };

    fn manager() -> Option<Manager> {
        Manager::RequestAsync().ok()?.get().ok()
    }

    /// A TimeSpan counts 100 ns ticks.
    fn ms(ticks: i64) -> u64 {
        (ticks.max(0) / 10_000) as u64
    }

    /// The cover picture of what is playing, as a data: URL (at most 2 MB), or "".
    fn art(props: &windows::Media::Control::GlobalSystemMediaTransportControlsSessionMediaProperties) -> String {
        use base64::Engine as _;
        use windows::Storage::Streams::DataReader;
        let read = || -> Option<String> {
            let stream = props.Thumbnail().ok()?.OpenReadAsync().ok()?.get().ok()?;
            let size = stream.Size().ok()?;
            if size == 0 || size > 2_000_000 {
                return None;
            }
            let reader = DataReader::CreateDataReader(&stream.GetInputStreamAt(0).ok()?).ok()?;
            reader.LoadAsync(size as u32).ok()?.get().ok()?;
            let mut buf = vec![0u8; size as usize];
            reader.ReadBytes(&mut buf).ok()?;
            let kind = stream.ContentType().map(|h| h.to_string()).unwrap_or_default();
            let kind = if kind.starts_with("image/") { kind } else { "image/png".into() };
            Some(format!("data:{kind};base64,{}", base64::engine::general_purpose::STANDARD.encode(&buf)))
        };
        read().unwrap_or_default()
    }

    fn read(s: &Session, last: Option<&NowPlaying>) -> Option<NowPlaying> {
        let props = s.TryGetMediaPropertiesAsync().ok()?.get().ok()?;
        let info = s.GetPlaybackInfo().ok()?;
        let controls = info.Controls().ok()?;
        let tl = s.GetTimelineProperties().ok()?;
        let playing = info.PlaybackStatus().ok()? == Status::Playing;
        let mut position = ms(tl.Position().ok()?.Duration);
        let duration = ms(tl.EndTime().ok()?.Duration - tl.StartTime().ok()?.Duration);
        // Windows only refreshes the position now and then; move it on by the time since.
        if playing {
            if let Ok(updated) = tl.LastUpdatedTime() {
                let now = windows::Foundation::DateTime { UniversalTime: now_ticks() };
                let since = ms(now.UniversalTime - updated.UniversalTime);
                if since < 6 * 3600 * 1000 {
                    position += since;
                }
            }
        }
        if duration > 0 {
            position = position.min(duration);
        }
        let title = props.Title().map(|h| h.to_string()).unwrap_or_default();
        let artist = props.Artist().map(|h| h.to_string()).unwrap_or_default();
        // The picture only changes with the song: read it once per song.
        let art = match last {
            Some(l) if l.title == title && l.artist == artist && !l.art.is_empty() => l.art.clone(),
            _ => art(&props),
        };
        Some(NowPlaying {
            app: s.SourceAppUserModelId().map(|h| h.to_string()).unwrap_or_default(),
            title,
            artist,
            art,
            album: props.AlbumTitle().map(|h| h.to_string()).unwrap_or_default(),
            playing,
            position_ms: position,
            duration_ms: duration,
            can_next: controls.IsNextEnabled().unwrap_or(false),
            can_prev: controls.IsPreviousEnabled().unwrap_or(false),
        })
    }

    /// Now, as a Windows FILETIME (100 ns since 1601), like DateTime::UniversalTime.
    fn now_ticks() -> i64 {
        const EPOCH_DIFF_SECS: i64 = 11_644_473_600;
        let d = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        (d.as_secs() as i64 + EPOCH_DIFF_SECS) * 10_000_000 + d.subsec_nanos() as i64 / 100
    }

    pub fn run(app: AppHandle) {
        // WinRT on this thread: join the multithreaded apartment first.
        unsafe {
            let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED);
        }
        let mut mgr = None;
        let mut last: Option<NowPlaying> = None;
        loop {
            if mgr.is_none() {
                mgr = manager();
            }
            let now = mgr
                .as_ref()
                .and_then(|m| m.GetCurrentSession().ok())
                .and_then(|s| read(&s, last.as_ref()))
                .filter(|n| !n.title.is_empty());
            // The position moves every second: send when anything else changes, and the
            // position only every few seconds (the island moves it on by itself).
            let changed = match (&last, &now) {
                (None, None) => false,
                (Some(a), Some(b)) => {
                    let same = NowPlaying { position_ms: 0, ..a.clone() } == NowPlaying { position_ms: 0, ..b.clone() };
                    !same || a.position_ms.abs_diff(b.position_ms) > 2500 || !b.playing && a.position_ms != b.position_ms
                }
                _ => true,
            };
            if changed {
                let _ = app.emit_to(island::WINDOW_LABEL, "media", now.clone());
                last = now;
            } else if let (Some(l), Some(n)) = (&mut last, &now) {
                // Keep our idea of the position current without sending it.
                l.position_ms = n.position_ms;
            }
            std::thread::sleep(Duration::from_millis(1000));
        }
    }

    pub fn control(action: &str) -> Result<(), String> {
        let s = manager()
            .and_then(|m| m.GetCurrentSession().ok())
            .ok_or("Nothing is playing.")?;
        let op = match action {
            "play_pause" => s.TryTogglePlayPauseAsync(),
            "next" => s.TrySkipNextAsync(),
            "prev" => s.TrySkipPreviousAsync(),
            other => return Err(format!("unknown media action {other}")),
        };
        op.and_then(|o| o.get()).map(|_| ()).map_err(|e| e.to_string())
    }
}
