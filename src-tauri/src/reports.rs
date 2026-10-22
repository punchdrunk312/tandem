//! Automatic error reports, only when the person has said yes (asked once; the switch
//! is in Settings). When something goes wrong, the problem and the last lines of the
//! app's own log are sent to cubecursor.shop, after everything personal has been taken
//! out: network addresses, computer names, user folders and web addresses are replaced
//! with placeholders, and the computer is known only by a one-way code.
//!
//! Nothing about what people type, copy or send ever reaches the log in the first place.

use sha2::{Digest, Sha256};
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

const ENDPOINT: &str = "https://cubecursor.shop/api/reports";
/// Log lines kept for context (after scrubbing).
const RING: usize = 60;
/// At most this many reports a day from one computer, and one per kind of problem an hour.
const PER_DAY: usize = 10;
const PER_KIND: Duration = Duration::from_secs(3600);

#[derive(serde::Serialize, serde::Deserialize, Clone)]
struct Report {
    device: String,
    version: String,
    os: String,
    kind: String,
    message: String,
    log: String,
}

struct Inner {
    ring: VecDeque<String>,
    queue: Vec<Report>,
    last_kind: HashMap<String, Instant>,
    day: (u64, usize),
    names: Vec<String>,
    device: String,
}

struct Reporter {
    on: AtomicBool,
    inner: Mutex<Inner>,
    pending: PathBuf,
    version: String,
    os: String,
}

static R: OnceLock<Reporter> = OnceLock::new();

/// Starts the reporter: `cfg_dir` holds reports that couldn't be sent yet (a crash, no
/// network) so they go out next time.
pub fn start(cfg_dir: &std::path::Path, version: &str, on: bool) {
    let pending = cfg_dir.join("reports-pending.json");
    let queue: Vec<Report> = std::fs::read(&pending)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    let r = Reporter {
        on: AtomicBool::new(on),
        inner: Mutex::new(Inner {
            ring: VecDeque::new(),
            queue,
            last_kind: HashMap::new(),
            day: (0, 0),
            names: Vec::new(),
            device: String::new(),
        }),
        pending,
        version: version.to_string(),
        os: format!("{} {}", std::env::consts::OS, std::env::consts::ARCH),
    };
    if R.set(r).is_err() {
        return;
    }
    // A crash: note it, and keep it on disk so it is sent when the app runs again.
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        log::error!("panic: {info}");
        save_pending();
        prev(info);
    }));
    let _ = std::thread::Builder::new()
        .name("cubecursor-reports".into())
        .spawn(sender);
}

pub fn set_enabled(on: bool) {
    if let Some(r) = R.get() {
        r.on.store(on, Relaxed);
        if !on {
            let mut i = r.inner.lock().unwrap();
            i.queue.clear();
            drop(i);
            let _ = std::fs::remove_file(&r.pending);
        }
    }
}

/// The names to hide (this computer's and the paired ones'), and this computer's id,
/// which only ever leaves as a one-way code.
pub fn set_identity(device_id: &str, names: Vec<String>) {
    if let Some(r) = R.get() {
        let mut i = r.inner.lock().unwrap();
        let mut h = Sha256::new();
        h.update(b"cubecursor-report:");
        h.update(device_id.as_bytes());
        i.device = h
            .finalize()
            .iter()
            .take(8)
            .map(|b| format!("{b:02x}"))
            .collect();
        i.names = names.into_iter().filter(|n| n.len() >= 2).collect();
        // Longest first, so "PC-2-Office" is hidden before "PC-2".
        i.names.sort_by_key(|n| std::cmp::Reverse(n.len()));
    }
}

/// Every line the app logs passes through here.
pub fn note(level: log::Level, line: &str) {
    let Some(r) = R.get() else { return };
    // try_lock: a crash while this very lock is held must not hang the crash report.
    let Ok(mut i) = r.inner.try_lock() else {
        return;
    };
    let clean = scrub(line, &i.names);
    let stamped = format!("{level} {clean}");
    i.ring.push_back(stamped);
    while i.ring.len() > RING {
        i.ring.pop_front();
    }
    if !r.on.load(Relaxed) || !is_problem(level, &clean) {
        return;
    }
    let kind = kind_of(&clean);
    let now = Instant::now();
    if i.last_kind
        .get(&kind)
        .is_some_and(|t| now.duration_since(*t) < PER_KIND)
    {
        return;
    }
    let today = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() / 86_400)
        .unwrap_or(0);
    if i.day.0 != today {
        i.day = (today, 0);
    }
    if i.day.1 >= PER_DAY {
        return;
    }
    i.day.1 += 1;
    i.last_kind.insert(kind.clone(), now);
    let report = Report {
        device: i.device.clone(),
        version: r.version.clone(),
        os: r.os.clone(),
        kind,
        message: clean,
        log: i.ring.iter().cloned().collect::<Vec<_>>().join("\n"),
    };
    if i.queue.len() < 20 {
        i.queue.push(report);
    }
}

fn save_pending() {
    if let Some(r) = R.get() {
        if let Ok(i) = r.inner.try_lock() {
            if i.queue.is_empty() {
                let _ = std::fs::remove_file(&r.pending);
            } else if let Ok(b) = serde_json::to_vec(&i.queue) {
                let _ = std::fs::write(&r.pending, b);
            }
        }
    }
}

fn sender() {
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(15))
        .build();
    loop {
        std::thread::sleep(Duration::from_secs(30));
        let Some(r) = R.get() else { continue };
        if !r.on.load(Relaxed) {
            continue;
        }
        let batch: Vec<Report> = std::mem::take(&mut r.inner.lock().unwrap().queue);
        if batch.is_empty() {
            continue;
        }
        let mut failed = Vec::new();
        for rep in batch {
            if agent.post(ENDPOINT).send_json(&rep).is_err() {
                failed.push(rep);
            }
        }
        {
            let mut i = r.inner.lock().unwrap();
            failed.append(&mut i.queue);
            failed.truncate(20);
            i.queue = failed;
        }
        save_pending();
    }
}

/// Lines that mean something went wrong (and not the everyday ones, like the other
/// computer being switched off).
fn is_problem(level: log::Level, line: &str) -> bool {
    if matches!(level, log::Level::Error | log::Level::Warn) {
        return true;
    }
    let l = line.to_ascii_lowercase();
    if l.starts_with("dial failed")
        || l.contains("under the press")
        || l.contains("not on the title bar")
        || l.contains("no window is ready")
        || l.contains("person said no")
    {
        return false;
    }
    [
        "could not",
        "couldn't",
        "can't",
        "failed",
        "refused",
        "isn't installed",
        "no way found",
        "still running",
        "too big",
        "panic",
        "connection lost",
    ]
    .iter()
    .any(|w| l.contains(w))
}

/// Groups reports: the first words of the line, with numbers and sizes taken out.
fn kind_of(line: &str) -> String {
    line.split_whitespace()
        .take(5)
        .map(|w| {
            if w.chars().any(|c| c.is_ascii_digit()) {
                "#"
            } else {
                w
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(80)
        .collect()
}

/// Takes the personal bits out of a log line.
pub fn scrub(line: &str, names: &[String]) -> String {
    let mut s = line.to_string();
    for n in names {
        if !n.is_empty() {
            s = replace_ci(&s, n, "[pc]");
        }
    }
    s = mask_ips(&s);
    s = mask_user_dirs(&s);
    s = mask_urls(&s);
    mask_ids(&s)
}

fn replace_ci(s: &str, needle: &str, with: &str) -> String {
    let ls = s.to_lowercase();
    let ln = needle.to_lowercase();
    if ln.is_empty() || ls.len() != s.len() {
        return s.replace(needle, with);
    }
    let mut out = String::new();
    let mut i = 0;
    while let Some(p) = ls[i..].find(&ln) {
        out.push_str(&s[i..i + p]);
        out.push_str(with);
        i += p + ln.len();
    }
    out.push_str(&s[i..]);
    out
}

/// 192.168.1.20:24800 -> [ip]
fn mask_ips(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = String::new();
    let mut i = 0;
    while i < b.len() {
        let start = i;
        let mut j = i;
        let mut dots = 0;
        let mut ok = true;
        for part in 0..4 {
            let d0 = j;
            while j < b.len() && b[j].is_ascii_digit() && j - d0 < 3 {
                j += 1;
            }
            if j == d0 {
                ok = false;
                break;
            }
            if part < 3 {
                if j < b.len() && b[j] == b'.' {
                    dots += 1;
                    j += 1;
                } else {
                    ok = false;
                    break;
                }
            }
        }
        let boundary_before = start == 0 || !b[start - 1].is_ascii_alphanumeric();
        if ok && dots == 3 && boundary_before && (j == b.len() || !b[j].is_ascii_digit()) {
            if j < b.len() && b[j] == b':' {
                j += 1;
                while j < b.len() && b[j].is_ascii_digit() {
                    j += 1;
                }
            }
            out.push_str("[ip]");
            i = j;
        } else {
            let ch = s[i..].chars().next().unwrap();
            out.push(ch);
            i += ch.len_utf8();
        }
    }
    out
}

/// C:\Users\Nikolas\Desktop -> C:\Users\[user]\Desktop, and the same for /Users/ and /home/.
fn mask_user_dirs(s: &str) -> String {
    let mut out = s.to_string();
    for marker in ["\\Users\\", "/Users/", "/home/"] {
        let mut from = 0;
        while let Some(p) = out[from..].find(marker) {
            let a = from + p + marker.len();
            let rest = &out[a..];
            let end = rest.find(['\\', '/', ' ', '"', '\'']).unwrap_or(rest.len());
            out = format!("{}[user]{}", &out[..a], &out[a + end..]);
            from = a + "[user]".len();
        }
    }
    out
}

/// https://site.com/watch?v=… -> https://site.com/…
fn mask_urls(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(p) = rest.find("http") {
        let (head, tail) = rest.split_at(p);
        out.push_str(head);
        let scheme = if tail.starts_with("https://") {
            "https://"
        } else if tail.starts_with("http://") {
            "http://"
        } else {
            out.push_str("http");
            rest = &tail[4..];
            continue;
        };
        let after = &tail[scheme.len()..];
        let end = after.find(char::is_whitespace).unwrap_or(after.len());
        let url = &after[..end];
        let host = url.split(['/', '?', '#']).next().unwrap_or("");
        out.push_str(scheme);
        out.push_str(host);
        if url.len() > host.len() {
            out.push_str("/…");
        }
        rest = &after[end..];
    }
    out.push_str(rest);
    out
}

/// Long hex ids (the paired computers' ids) -> [id]
fn mask_ids(s: &str) -> String {
    s.split(' ')
        .map(|w| {
            let core = w.trim_matches(|c: char| !c.is_ascii_alphanumeric());
            if core.len() >= 12 && core.chars().all(|c| c.is_ascii_hexdigit()) {
                w.replace(core, "[id]")
            } else {
                w.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn personal_bits_are_taken_out() {
        let names = vec!["DESKTOP-GFJL3OO".to_string(), "MYTHICALS".to_string()];
        let s = scrub(
            "app move: 304efa5654ca0cfb asked to open chrome.exe from 172.20.10.9:24800 (mythicals) C:\\Users\\Nikolas\\Desktop\\a.txt https://youtube.com/watch?v=abc",
            &names,
        );
        assert!(!s.contains("304efa"), "{s}");
        assert!(!s.contains("172.20"), "{s}");
        assert!(!s.to_lowercase().contains("mythicals"), "{s}");
        assert!(!s.contains("Nikolas"), "{s}");
        assert!(!s.contains("watch?v"), "{s}");
        assert!(s.contains("chrome.exe"), "{s}");
        assert!(s.contains("https://youtube.com/…"), "{s}");
        assert!(
            s.contains("[ip]") && s.contains("[pc]") && s.contains("[id]") && s.contains("[user]")
        );
    }

    #[test]
    fn version_numbers_are_not_mistaken_for_addresses() {
        assert_eq!(mask_ips("v1.2.3 and 1.2.3.4"), "v1.2.3 and [ip]");
        assert_eq!(mask_ips("10.0.0.1"), "[ip]");
    }

    #[test]
    fn only_real_problems_count() {
        use log::Level::*;
        assert!(is_problem(Warn, "clipboard: format 8 was refused"));
        assert!(is_problem(
            Info,
            "app move: no way found to start Discord.exe"
        ));
        assert!(!is_problem(Info, "dial failed: io: connection timed out"));
        assert!(!is_problem(
            Info,
            "app drag: the window under the press can't be moved"
        ));
        assert!(!is_problem(Info, "clipboard: sending a picture (20 KB)"));
    }

    #[test]
    fn kinds_group_the_same_problem() {
        assert_eq!(
            kind_of("clipboard: picture too big to sync (13000 KB)"),
            kind_of("clipboard: picture too big to sync (9000 KB)")
        );
    }
}
