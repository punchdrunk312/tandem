//! What is playing (Spotify, a browser, any app that talks to Windows' media controls), and
//! the play/pause/next/previous buttons for it. Polled once a second on its own thread and
//! sent to the island as "media"; nothing is sent while nothing changes.
//!
//! Lyrics are optional (off unless the person turns them on in the bar): the song's title,
//! artist, album and length go to lrclib.net and NetEase Cloud Music (timed lyrics), or,
//! when neither has them, the title and artist to lyrics.ovh (plain lyrics). Nothing else
//! is sent.

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
    /// The player lets us jump to a spot in the song.
    pub can_seek: bool,
    /// A YouTube video in a browser whose window is minimised (the bar can play it on).
    pub video_hidden: bool,
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

/// "play_pause", "next", "prev", or "seek:<ms>" for the app that is playing.
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

/// The program behind a player's id ("Spotify.exe", "Chrome", "MSEdge", an app's
/// "…!Spotify"), as the Volume Mixer knows it.
fn exe_hint(app: &str) -> String {
    let a = app.to_lowercase();
    for (k, exe) in [("spotify", "spotify"), ("chrome", "chrome"), ("msedge", "msedge"), ("firefox", "firefox"), ("308046b0af4a39cb", "firefox"), ("brave", "brave"), ("opera", "opera"), ("vlc", "vlc"), ("zune", "microsoft.media.player"), ("music", "microsoft.media.player")] {
        if a.contains(k) {
            return exe.into();
        }
    }
    a.rsplit('!').next().unwrap_or(&a).trim_end_matches(".exe").to_string()
}

/// Speakers, headphones…: where the sound can go, and which is used now.
#[tauri::command]
pub fn media_outputs() -> Vec<tandem_core::audio::Output> {
    tandem_core::audio::outputs()
}
#[tauri::command]
pub fn media_set_output(id: String) -> Result<(), String> {
    tandem_core::audio::set_output(&id)
}
/// The playing app's own volume (0..1), as in the Volume Mixer.
#[tauri::command]
pub fn media_volume(app: String) -> Option<f32> {
    tandem_core::audio::app_volume(&exe_hint(&app))
}
#[tauri::command]
pub fn media_set_volume(app: String, volume: f32) -> Result<(), String> {
    tandem_core::audio::set_app_volume(&exe_hint(&app), volume)
}

/// Time-stamped lyrics ("[mm:ss.xx] line" per line), or plain lyrics, or None.
/// Runs off the UI thread: Tauri runs async commands on its own pool.
#[tauri::command]
pub async fn media_lyrics(title: String, artist: String, album: String, duration_ms: u64) -> Result<Option<Lyrics>, String> {
    tauri::async_runtime::spawn_blocking(move || lyrics(&title, &artist, &album, duration_ms))
        .await
        .map_err(|e| e.to_string())?
}

/// The YouTube video with this title (and channel), found by a YouTube search: its id.
#[tauri::command]
pub async fn media_video_id(title: String, artist: String) -> Result<Option<String>, String> {
    tauri::async_runtime::spawn_blocking(move || video_id(&title, &artist))
        .await
        .map_err(|e| e.to_string())?
}

/// Brings the minimised browser window with the video back.
#[tauri::command]
pub fn media_show_video(title: String) {
    #[cfg(windows)]
    sys::show_video(&title);
    #[cfg(not(windows))]
    let _ = title;
}

fn video_id(title: &str, artist: &str) -> Result<Option<String>, String> {
    let page = ureq::AgentBuilder::new()
        .timeout(std::time::Duration::from_secs(8))
        .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0 Safari/537.36")
        .build()
        .get("https://www.youtube.com/results")
        .query("search_query", &format!("{title} {artist}"))
        // Skips the cookie question YouTube asks in Europe.
        .set("Cookie", "SOCS=CAI; CONSENT=YES+1")
        .call()
        .map_err(|e| format!("Couldn't reach YouTube: {e}"))?
        .into_string()
        .map_err(|e| e.to_string())?;
    let want = squash(title);
    let key = "\"videoRenderer\":{\"videoId\":\"";
    let mut first = None;
    for (i, _) in page.match_indices(key) {
        let id: String = page[i + key.len()..].chars().take_while(|c| *c != '"').collect();
        if id.len() != 11 {
            continue;
        }
        first.get_or_insert(id.clone());
        // The result whose title is this one (not an ad or a similar video).
        let near = &page[i..(i + 3000).min(page.len())];
        if !want.is_empty() && squash(near).contains(&want) {
            return Ok(Some(id));
        }
    }
    Ok(first)
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
    // Players name songs "Song - Remastered 2011" or "Song (feat. X)" and list several
    // artists as "A, B"; the libraries know them as "Song" by "A". Try as given, then plain.
    let (t2, a2) = (plain_title(title), first_artist(artist));
    let mut tries = vec![(title.to_string(), artist.to_string())];
    if (t2.as_str(), a2.as_str()) != (title, artist) {
        tries.push((t2, a2));
    }
    let mut reached = false;
    let mut last_err = None;
    for (t, a) in &tries {
        match lrclib(t, a, album, duration_ms) {
            Ok(Some(l)) => return Ok(Some(l)),
            Ok(None) => reached = true,
            Err(e) => {
                // Down or unreachable: asking again with the plain title would only wait again.
                last_err = Some(e);
                break;
            }
        }
    }
    for (t, a) in &tries {
        match netease(t, a, duration_ms) {
            Ok(Some(l)) => return Ok(Some(l)),
            Ok(None) => reached = true,
            Err(e) => last_err = Some(e),
        }
    }
    for (t, a) in &tries {
        match lyrics_ovh(t, a) {
            Ok(Some(l)) => return Ok(Some(l)),
            Ok(None) => reached = true,
            Err(e) => last_err = Some(e),
        }
    }
    // "Not found" if any library answered; "can't reach" only if none did.
    match last_err {
        Some(e) if !reached => Err(e),
        _ => Ok(None),
    }
}

/// "Song - Remastered 2011", "Song (feat. X)", "Song [Live]" → "Song".
fn plain_title(t: &str) -> String {
    let mut s = t.to_string();
    for cut in [" - ", " (", " [", " feat.", " ft."] {
        if let Some(i) = s.find(cut) {
            if i > 0 {
                s.truncate(i);
            }
        }
    }
    s.trim().to_string()
}

/// "A, B & C" or "A feat. B" → "A".
fn first_artist(a: &str) -> String {
    let mut s = a.to_string();
    for cut in [",", " & ", " feat.", " ft.", " x ", ";", "/"] {
        if let Some(i) = s.find(cut) {
            if i > 0 {
                s.truncate(i);
            }
        }
    }
    s.trim().to_string()
}

pub(crate) fn agent() -> ureq::Agent {
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
    let enc = |s: &str| s.trim().replace(['/', '?'], " ");
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

/// Timed lyrics from NetEase Cloud Music: the closest match by name, artist and length.
fn netease(title: &str, artist: &str, duration_ms: u64) -> Result<Option<Lyrics>, String> {
    let fail = |e: ureq::Error| format!("Could not reach the lyrics library: {e}");
    let found: serde_json::Value = agent()
        .get("https://music.163.com/api/search/get")
        .query("s", &format!("{title} {artist}"))
        .query("type", "1")
        .query("limit", "8")
        .call()
        .map_err(fail)?
        .into_json()
        .map_err(|e| e.to_string())?;
    let want_t = squash(&plain_title(title));
    let want_a = squash(&first_artist(artist));
    let mut best: Option<(u64, u64)> = None; // (how far off in length, song id)
    for s in found["result"]["songs"].as_array().into_iter().flatten() {
        let name = squash(&plain_title(s["name"].as_str().unwrap_or("")));
        if name.is_empty() || !(name.contains(&want_t) || want_t.contains(&name)) {
            continue;
        }
        let artists: Vec<String> = s["artists"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|a| a["name"].as_str().map(squash))
            .collect();
        if !want_a.is_empty() && !artists.iter().any(|a| a.contains(&want_a) || want_a.contains(a.as_str())) {
            continue;
        }
        let off = if duration_ms > 0 {
            s["duration"].as_u64().unwrap_or(0).abs_diff(duration_ms)
        } else {
            0
        };
        // Another version (live, a remix) runs to a different length: its lyrics won't fit.
        if off > 4000 {
            continue;
        }
        let Some(id) = s["id"].as_u64() else { continue };
        if best.is_none_or(|(o, _)| off < o) {
            best = Some((off, id));
        }
    }
    let Some((_, id)) = best else { return Ok(None) };
    let body: serde_json::Value = agent()
        .get("https://music.163.com/api/song/lyric")
        .query("id", &id.to_string())
        .query("lv", "1")
        .call()
        .map_err(fail)?
        .into_json()
        .map_err(|e| e.to_string())?;
    let text = clean_lrc(body["lrc"]["lyric"].as_str().unwrap_or(""));
    let sung = text.lines().filter(|l| !l.rsplit(']').next().unwrap_or("").trim().is_empty()).count();
    Ok((sung >= 3).then_some(Lyrics { synced: true, text }))
}

/// Lower case, letters and digits only: "Don't Stop Me Now" → "dontstopmenow".
fn squash(s: &str) -> String {
    s.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

/// Drops the credit lines some libraries put first ("[00:00.00] 作词 : …", "Producer: …").
fn clean_lrc(text: &str) -> String {
    let credit = |words: &str| {
        let Some(i) = words.find([':', '：']) else { return false };
        let key = words[..i].trim();
        let cjk = key.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c));
        let known = ["producer", "lyrics", "composer", "written by", "arranger", "mixed by"]
            .iter()
            .any(|k| key.eq_ignore_ascii_case(k));
        key.chars().count() <= 12 && (cjk || known)
    };
    text.lines()
        .filter(|l| {
            let words = l.rsplit(']').next().unwrap_or("");
            !credit(words) && !words.contains("纯音乐")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Timed lyrics from lrclib.net.
fn lrclib(title: &str, artist: &str, album: &str, duration_ms: u64) -> Result<Option<Lyrics>, String> {
    let agent = ureq::AgentBuilder::new()
        .timeout(std::time::Duration::from_secs(5))
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

    /// The player to show and control: never the bar itself (playing a video). One that is
    /// playing comes first (Windows' "current" one can be a paused app opened last), then
    /// the current one, then any.
    fn session(m: &Manager) -> Option<Session> {
        let own = std::env::current_exe()
            .ok()
            .and_then(|p| p.file_stem().map(|s| s.to_string_lossy().to_lowercase()))
            .unwrap_or_default();
        let ours = |s: &Session| {
            let id = s.SourceAppUserModelId().map(|h| h.to_string().to_lowercase()).unwrap_or_default();
            id.contains("cubecursor") || id.contains("webview") || (!own.is_empty() && id.contains(&own))
        };
        let playing = |s: &Session| {
            s.GetPlaybackInfo()
                .and_then(|i| i.PlaybackStatus())
                .is_ok_and(|st| st == Status::Playing)
        };
        let all: Vec<Session> = m.GetSessions().ok()?.into_iter().filter(|s| !ours(s)).collect();
        let current = m.GetCurrentSession().ok().filter(|s| !ours(s));
        if let Some(c) = current.as_ref().filter(|c| playing(c)) {
            return Some(c.clone());
        }
        all.iter().find(|s| playing(s)).cloned().or(current).or_else(|| all.into_iter().next())
    }

    /// The browser window showing the YouTube video with this title, and whether it is minimised.
    pub fn video_window(title: &str) -> Option<(isize, bool)> {
        use windows::core::BOOL;
        use windows::Win32::Foundation::{HWND, LPARAM};
        use windows::Win32::UI::WindowsAndMessaging::{EnumWindows, GetWindowTextW, IsIconic, IsWindowVisible};
        struct Find {
            title: String,
            found: Option<(isize, bool)>,
        }
        unsafe extern "system" fn each(h: HWND, lp: LPARAM) -> BOOL {
            let f = &mut *(lp.0 as *mut Find);
            if !IsWindowVisible(h).as_bool() {
                return BOOL(1);
            }
            let mut buf = [0u16; 512];
            let n = GetWindowTextW(h, &mut buf);
            let text = String::from_utf16_lossy(&buf[..n.max(0) as usize]);
            if text.contains(" - YouTube") && text.contains(&f.title) {
                f.found = Some((h.0 as isize, IsIconic(h).as_bool()));
                return BOOL(0);
            }
            BOOL(1)
        }
        if title.trim().is_empty() {
            return None;
        }
        let mut f = Find { title: title.to_string(), found: None };
        unsafe {
            let _ = EnumWindows(Some(each), LPARAM(&mut f as *mut Find as isize));
        }
        f.found
    }

    pub fn show_video(title: &str) {
        use windows::Win32::Foundation::HWND;
        use windows::Win32::UI::WindowsAndMessaging::{SetForegroundWindow, ShowWindow, SW_RESTORE};
        if let Some((h, _)) = video_window(title) {
            let h = HWND(h as *mut core::ffi::c_void);
            unsafe {
                let _ = ShowWindow(h, SW_RESTORE);
                let _ = SetForegroundWindow(h);
            }
        }
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

    fn read(s: &Session, last: Option<&NowPlaying>, art_retry: bool) -> Option<NowPlaying> {
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
            // Same song: the same picture (one without a cover is looked at again now and then).
            Some(l) if l.title == title && l.artist == artist && (!l.art.is_empty() || !art_retry) => l.art.clone(),
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
            can_seek: controls.IsPlaybackPositionEnabled().unwrap_or(false),
            video_hidden: false,
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
        // Where the island thinks the song is: the position sent, and when.
        let mut sent = (0u64, std::time::Instant::now());
        let mut polls = 0u32;
        loop {
            polls = polls.wrapping_add(1);
            if mgr.is_none() {
                mgr = manager();
            }
            let now = mgr
                .as_ref()
                .and_then(session)
                .and_then(|s| read(&s, last.as_ref(), polls.is_multiple_of(12)))
                .filter(|n| !n.title.is_empty())
                .map(|mut n| {
                    let app = n.app.to_lowercase();
                    let browser = ["chrome", "edge", "firefox", "brave", "opera", "vivaldi"].iter().any(|b| app.contains(b));
                    n.video_hidden = browser && video_window(&n.title).is_some_and(|(_, min)| min);
                    n
                });
            // The island moves the position on by itself: send when anything else changes,
            // or when its idea of the position has drifted (a jump, a stall, a late start),
            // so the lyrics stay on the beat.
            let changed = match (&last, &now) {
                (None, None) => false,
                (Some(a), Some(b)) => {
                    let same = NowPlaying { position_ms: 0, ..a.clone() } == NowPlaying { position_ms: 0, ..b.clone() };
                    let thinks = sent.0 + if b.playing { sent.1.elapsed().as_millis() as u64 } else { 0 };
                    !same || b.duration_ms > 0 && thinks.abs_diff(b.position_ms) > 250
                }
                _ => true,
            };
            if changed {
                let _ = app.emit_to(island::WINDOW_LABEL, "media", now.clone());
                if let Some(n) = &now {
                    sent = (n.position_ms, std::time::Instant::now());
                }
                last = now;
            }
            std::thread::sleep(Duration::from_millis(400));
        }
    }

    pub fn control(action: &str) -> Result<(), String> {
        let s = manager().as_ref().and_then(session).ok_or("Nothing is playing.")?;
        let op = match action {
            "play_pause" => s.TryTogglePlayPauseAsync(),
            "pause" => s.TryPauseAsync(),
            "play" => s.TryPlayAsync(),
            "next" => s.TrySkipNextAsync(),
            "prev" => s.TrySkipPreviousAsync(),
            seek if seek.starts_with("seek:") => {
                let ms: i64 = seek[5..].parse().map_err(|_| "bad seek")?;
                let start = s.GetTimelineProperties().and_then(|t| t.StartTime()).map(|t| t.Duration).unwrap_or(0);
                let ok = s
                    .TryChangePlaybackPositionAsync(start + ms.max(0) * 10_000)
                    .and_then(|o| o.get())
                    .map_err(|e| e.to_string())?;
                return if ok { Ok(()) } else { Err("This player doesn't let CubeCursor jump around in the song.".into()) };
            }
            rate if rate.starts_with("rate:") => {
                let r: f64 = rate[5..].parse().map_err(|_| "bad rate")?;
                let ok = s.TryChangePlaybackRateAsync(r).and_then(|o| o.get()).map_err(|e| e.to_string())?;
                return if ok { Ok(()) } else { Err("This player doesn't let CubeCursor change its speed.".into()) };
            }
            other => return Err(format!("unknown media action {other}")),
        };
        op.and_then(|o| o.get()).map(|_| ()).map_err(|e| e.to_string())
    }
}

