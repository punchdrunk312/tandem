//! File transfer: manifest building, receive-side sanitising, streaming.

use crate::channel::{SecureReader, SecureWriter};
use crate::error::{Error, Result};
use crate::proto::{FileItem, FileMsg};
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use tokio::fs::File;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncSeekExt, AsyncWrite, AsyncWriteExt};

pub const CHUNK: usize = 64 * 1024;

/// Set while this computer is being controlled / is controlling the other one: file
/// sends slow down so a big transfer never competes with the mouse for the Wi-Fi.
pub static SLOW_DOWN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
const SLOW_BYTES_PER_SEC: u64 = 6 * 1024 * 1024;
pub const MAX_ITEMS: usize = 20_000;

/// What the sender keeps for an outstanding offer.
pub struct Offer {
    pub items: Vec<FileItem>,
    pub paths: Vec<PathBuf>,
}

/// Walk the given files/folders into a flat manifest. Symlinks are skipped
/// (never followed) so an offer cannot be tricked into exposing other files.
pub fn build_manifest(roots: &[PathBuf]) -> std::io::Result<Offer> {
    let mut items = Vec::new();
    let mut paths = Vec::new();
    fn walk(
        abs: &Path,
        rel: String,
        items: &mut Vec<FileItem>,
        paths: &mut Vec<PathBuf>,
    ) -> std::io::Result<()> {
        if items.len() >= MAX_ITEMS {
            return Err(std::io::Error::other("too many files"));
        }
        let md = std::fs::symlink_metadata(abs)?;
        if md.file_type().is_symlink() {
            return Ok(());
        }
        if md.is_dir() {
            items.push(FileItem {
                path: rel.clone(),
                size: 0,
                is_dir: true,
            });
            paths.push(abs.to_path_buf());
            let mut kids: Vec<_> = std::fs::read_dir(abs)?.filter_map(|e| e.ok()).collect();
            kids.sort_by_key(|e| e.file_name());
            for k in kids {
                let name = k.file_name().to_string_lossy().into_owned();
                walk(&k.path(), format!("{rel}/{name}"), items, paths)?;
            }
        } else if md.is_file() {
            items.push(FileItem {
                path: rel,
                size: md.len(),
                is_dir: false,
            });
            paths.push(abs.to_path_buf());
        }
        Ok(())
    }
    for r in roots {
        let name = r
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .ok_or_else(|| std::io::Error::other("path has no file name"))?;
        walk(r, name, &mut items, &mut paths)?;
    }
    Ok(Offer { items, paths })
}

pub fn total_size(items: &[FileItem]) -> u64 {
    items.iter().map(|i| i.size).sum()
}

const RESERVED: [&str; 22] = [
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

fn component_ok(c: &str) -> bool {
    if c.is_empty() || c == "." || c == ".." || c.len() > 255 {
        return false;
    }
    if c.chars()
        .any(|ch| ch.is_control() || matches!(ch, '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|'))
    {
        return false;
    }
    if c.ends_with('.') || c.ends_with(' ') {
        return false;
    }
    let stem = c.split('.').next().unwrap_or(c).to_ascii_uppercase();
    !RESERVED.contains(&stem.as_str())
}

/// Turn a peer-supplied relative path into a safe relative `PathBuf`, or `None`.
pub fn sanitize_rel(p: &str) -> Option<PathBuf> {
    if p.starts_with('/') {
        return None;
    }
    let mut out = PathBuf::new();
    for c in p.split('/') {
        if !component_ok(c) {
            return None;
        }
        out.push(c);
    }
    if out.components().any(|c| !matches!(c, Component::Normal(_))) {
        return None;
    }
    Some(out)
}

/// `name`, or `name (1)`, `name (2)`… — whichever does not exist in `dir`.
pub fn unique_name(dir: &Path, name: &str) -> String {
    if !dir.join(name).exists() {
        return name.to_string();
    }
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    for n in 1.. {
        let cand = format!("{stem} ({n}){ext}");
        if !dir.join(&cand).exists() {
            return cand;
        }
    }
    unreachable!()
}

/// Moves received files (and folders) into `dir`, never overwriting: a clash gets a
/// `name (1)` style name. Falls back to copy-and-delete across drives.
/// Moves `paths` into `dir` (names made unique). Returns where each one went.
pub fn move_into(paths: &[PathBuf], dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    std::fs::create_dir_all(dir)?;
    let mut staging: Option<PathBuf> = None;
    let mut out = Vec::new();
    for p in paths {
        let Some(name) = p.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if staging.is_none() {
            staging = p.parent().map(Path::to_path_buf);
        }
        let target = dir.join(unique_name(dir, name));
        if std::fs::rename(p, &target).is_err() {
            copy_tree(p, &target)?;
            if p.is_dir() {
                std::fs::remove_dir_all(p)?;
            } else {
                std::fs::remove_file(p)?;
            }
        }
        out.push(target);
    }
    if let Some(s) = staging {
        let _ = std::fs::remove_dir(&s); // only if now empty
    }
    Ok(out)
}

/// A drag that stays on one computer: moves `paths` into `dir` (like Explorer does on the
/// same drive), and copies instead — leaving the originals — when a move is not possible.
pub fn move_or_copy(paths: &[PathBuf], dir: &Path) -> std::io::Result<()> {
    for p in paths {
        let Some(name) = p.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if dir.starts_with(p) {
            continue; // a folder dropped into itself
        }
        let target = dir.join(unique_name(dir, name));
        if std::fs::rename(p, &target).is_err() {
            copy_tree(p, &target)?;
        }
    }
    Ok(())
}

fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    if from.is_dir() {
        std::fs::create_dir_all(to)?;
        for e in std::fs::read_dir(from)? {
            let e = e?;
            copy_tree(&e.path(), &to.join(e.file_name()))?;
        }
    } else {
        std::fs::copy(from, to)?;
    }
    Ok(())
}

/// Sender side of a file connection. Returns the offer id (if the request was
/// read) together with the outcome.
pub async fn serve<R, W>(
    r: &mut SecureReader<R>,
    w: &mut SecureWriter<W>,
    lookup: impl Fn(u64) -> Option<std::sync::Arc<Offer>>,
    progress: &(dyn Fn(u64, u64) + Send + Sync),
    cancelled: &(dyn Fn(u64) -> bool + Send + Sync),
) -> (Option<u64>, Result<()>)
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let (id, from) = match r.recv::<FileMsg>().await {
        Ok(FileMsg::Request { id }) => (id, ResumePoint::default()),
        Ok(FileMsg::Resume { id, index, offset }) => (id, ResumePoint { index, offset }),
        Ok(_) => return (None, Err(Error::Proto("expected Request".into()))),
        Err(e) => return (None, Err(e)),
    };
    (Some(id), serve_offer(w, id, from, lookup, progress, cancelled).await)
}

/// How far a receive got: the item being written and how many of its bytes are on disk.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ResumePoint {
    pub index: u32,
    pub offset: u64,
}

/// Bytes of `items` before `at` (whole files before its item, plus its offset).
fn bytes_before(items: &[FileItem], at: ResumePoint) -> u64 {
    items
        .iter()
        .take(at.index as usize)
        .filter(|i| !i.is_dir)
        .map(|i| i.size)
        .sum::<u64>()
        + at.offset
}

/// Is this the kind of failure a dropped connection causes (so the transfer can carry on
/// later), rather than a refusal, a bad message or a local disk problem?
pub fn is_interruption(e: &Error) -> bool {
    use std::io::ErrorKind as K;
    match e {
        Error::Closed => true,
        Error::Io(io) => matches!(
            io.kind(),
            K::ConnectionReset
                | K::ConnectionAborted
                | K::ConnectionRefused
                | K::NotConnected
                | K::BrokenPipe
                | K::UnexpectedEof
                | K::TimedOut
        ),
        _ => false,
    }
}

async fn serve_offer<W: AsyncWrite + Unpin>(
    w: &mut SecureWriter<W>,
    id: u64,
    from: ResumePoint,
    lookup: impl Fn(u64) -> Option<std::sync::Arc<Offer>>,
    progress: &(dyn Fn(u64, u64) + Send + Sync),
    cancelled: &(dyn Fn(u64) -> bool + Send + Sync),
) -> Result<()> {
    let Some(offer) = lookup(id) else {
        w.send(&FileMsg::Error("offer expired".into())).await?;
        return Err(Error::Proto("unknown offer".into()));
    };
    if from.index as usize > offer.items.len() {
        w.send(&FileMsg::Error("cannot resume: bad position".into())).await?;
        return Err(Error::Proto("resume past the end".into()));
    }
    let mut sent = bytes_before(&offer.items, from);
    progress(id, sent);
    for (i, (item, path)) in offer.items.iter().zip(&offer.paths).enumerate() {
        if i < from.index as usize {
            continue;
        }
        w.send(&FileMsg::Header { index: i as u32 }).await?;
        if item.is_dir {
            continue;
        }
        let skip = if i == from.index as usize { from.offset } else { 0 };
        if skip > item.size {
            let _ = w
                .send(&FileMsg::Error(format!("{} changed while sending", item.path)))
                .await;
            return Err(Error::Proto("resume offset past the end of the file".into()));
        }
        let mut f = match File::open(path).await {
            Ok(f) => f,
            Err(e) => {
                let _ = w
                    .send(&FileMsg::Error(format!("cannot read {}: {e}", item.path)))
                    .await;
                return Err(e.into());
            }
        };
        if skip > 0 {
            f.seek(std::io::SeekFrom::Start(skip)).await?;
        }
        let mut left = item.size - skip;
        let mut buf = vec![0u8; CHUNK];
        while left > 0 {
            // The person cancelled this send: tell the other side and stop.
            if cancelled(id) {
                let _ = w.send(&FileMsg::Error("cancelled".into())).await;
                return Err(Error::Rejected("cancelled".into()));
            }
            let want = left.min(CHUNK as u64) as usize;
            let n = f.read(&mut buf[..want]).await?;
            if n == 0 {
                let _ = w
                    .send(&FileMsg::Error(format!(
                        "{} changed while sending",
                        item.path
                    )))
                    .await;
                return Err(Error::Proto("file shrank".into()));
            }
            w.send(&FileMsg::Data(buf[..n].to_vec())).await?;
            left -= n as u64;
            sent += n as u64;
            progress(id, sent);
            if SLOW_DOWN.load(std::sync::atomic::Ordering::Relaxed) {
                tokio::time::sleep(std::time::Duration::from_micros(
                    n as u64 * 1_000_000 / SLOW_BYTES_PER_SEC,
                ))
                .await;
            }
        }
        w.send(&FileMsg::End).await?;
    }
    w.send(&FileMsg::Done).await
}

/// Receiver side. Returns the top-level paths created inside `dest`.
pub async fn receive<R, W>(
    r: &mut SecureReader<R>,
    w: &mut SecureWriter<W>,
    id: u64,
    items: &[FileItem],
    dest: &Path,
    progress: &(dyn Fn(u64) + Send + Sync),
) -> Result<Vec<PathBuf>>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let plan = plan_receive(items, dest).await?;
    receive_planned(r, w, id, items, dest, &plan, None, progress, &|_| {}).await
}

/// Where each item of a receive goes inside its folder. Chosen once, before anything is
/// written, so a resumed receive keeps writing to the same files.
#[derive(Clone, Debug)]
pub struct ReceivePlan {
    rels: Vec<PathBuf>,
    tops: Vec<PathBuf>,
}

/// Validates the offer and picks collision-free top-level names inside `dest`.
pub async fn plan_receive(items: &[FileItem], dest: &Path) -> Result<ReceivePlan> {
    let mut renames: HashMap<String, String> = HashMap::new();
    let mut tops: Vec<String> = Vec::new();
    let mut rels: Vec<PathBuf> = Vec::with_capacity(items.len());
    tokio::fs::create_dir_all(dest).await?;
    for it in items {
        let rel = sanitize_rel(&it.path)
            .ok_or_else(|| Error::Proto(format!("unsafe path {:?}", it.path)))?;
        let mut comps = rel.components();
        let top = comps
            .next()
            .unwrap()
            .as_os_str()
            .to_string_lossy()
            .into_owned();
        if !renames.contains_key(&top) {
            renames.insert(top.clone(), unique_name(dest, &top));
            tops.push(top.clone());
        }
        let mut target = PathBuf::from(&renames[&top]);
        target.extend(comps);
        rels.push(target);
    }
    Ok(ReceivePlan {
        rels,
        tops: tops.iter().map(|t| PathBuf::from(&renames[t])).collect(),
    })
}

/// Receives into the places `plan` chose. With `from`, carries on an interrupted receive
/// (asking the sender to start there); `mark` hears how far it has got after every chunk.
#[allow(clippy::too_many_arguments)]
pub async fn receive_planned<R, W>(
    r: &mut SecureReader<R>,
    w: &mut SecureWriter<W>,
    id: u64,
    items: &[FileItem],
    dest: &Path,
    plan: &ReceivePlan,
    from: Option<ResumePoint>,
    progress: &(dyn Fn(u64) + Send + Sync),
    mark: &(dyn Fn(ResumePoint) + Send + Sync),
) -> Result<Vec<PathBuf>>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let rels = &plan.rels;
    let mut start = from.unwrap_or_default();
    if start.index as usize > items.len() {
        start = ResumePoint::default();
    }
    // Trust the disk, not memory: carry on from what is really there.
    if start.offset > 0 {
        let have = match items.get(start.index as usize) {
            Some(it) if !it.is_dir => tokio::fs::metadata(dest.join(&rels[start.index as usize]))
                .await
                .map(|m| m.len().min(it.size))
                .unwrap_or(0),
            _ => 0,
        };
        start.offset = start.offset.min(have);
    }
    if from.is_some() {
        w.send(&FileMsg::Resume {
            id,
            index: start.index,
            offset: start.offset,
        })
        .await?;
    } else {
        w.send(&FileMsg::Request { id }).await?;
    }
    let mut got_total = bytes_before(items, start);
    progress(got_total);
    for (i, it) in items.iter().enumerate().skip(start.index as usize) {
        match r.recv::<FileMsg>().await? {
            FileMsg::Header { index } if index as usize == i => {}
            FileMsg::Error(e) => return Err(Error::Rejected(e)),
            _ => return Err(Error::Proto("unexpected message".into())),
        }
        let target = dest.join(&rels[i]);
        if it.is_dir {
            tokio::fs::create_dir_all(&target).await?;
            continue;
        }
        if let Some(parent) = target.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let carry_on = i == start.index as usize && start.offset > 0;
        let mut f = if carry_on {
            let mut f = tokio::fs::OpenOptions::new().write(true).open(&target).await?;
            f.set_len(start.offset).await?;
            f.seek(std::io::SeekFrom::End(0)).await?;
            f
        } else {
            File::create(&target).await?
        };
        let mut got = if carry_on { start.offset } else { 0 };
        loop {
            match r.recv::<FileMsg>().await? {
                FileMsg::Data(b) => {
                    got += b.len() as u64;
                    if got > it.size {
                        return Err(Error::Proto("more data than announced".into()));
                    }
                    f.write_all(&b).await?;
                    got_total += b.len() as u64;
                    progress(got_total);
                    mark(ResumePoint {
                        index: i as u32,
                        offset: got,
                    });
                }
                FileMsg::End => break,
                FileMsg::Error(e) => return Err(Error::Rejected(e)),
                _ => return Err(Error::Proto("unexpected message".into())),
            }
        }
        if got != it.size {
            return Err(Error::Proto("size mismatch".into()));
        }
        f.flush().await?;
        mark(ResumePoint {
            index: i as u32 + 1,
            offset: 0,
        });
    }
    match r.recv::<FileMsg>().await? {
        FileMsg::Done => {}
        FileMsg::Error(e) => return Err(Error::Rejected(e)),
        _ => return Err(Error::Proto("expected Done".into())),
    }
    Ok(plan.tops.iter().map(|t| dest.join(t)).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize() {
        for ok in ["a.txt", "dir/a b.txt", "Ünï/ç.md"] {
            assert!(sanitize_rel(ok).is_some(), "{ok}");
        }
        for bad in [
            "",
            "/etc/passwd",
            "../x",
            "a/../../x",
            "a/./b",
            "a//b",
            "C:\\x",
            "a\\b",
            "c:",
            "a:b",
            "con",
            "NUL.txt",
            "dir/com1.log",
            "x.",
            "x ",
            "a\u{0}b",
            "a/\n",
        ] {
            assert!(sanitize_rel(bad).is_none(), "{bad:?}");
        }
    }

    #[test]
    fn unique() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(unique_name(d.path(), "a.txt"), "a.txt");
        std::fs::write(d.path().join("a.txt"), "").unwrap();
        assert_eq!(unique_name(d.path(), "a.txt"), "a (1).txt");
        std::fs::write(d.path().join("a (1).txt"), "").unwrap();
        assert_eq!(unique_name(d.path(), "a.txt"), "a (2).txt");
    }

    #[test]
    fn manifest_walks_folders_and_skips_nothing_else() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join("proj/sub")).unwrap();
        std::fs::write(d.path().join("proj/a.txt"), "hello").unwrap();
        std::fs::write(d.path().join("proj/sub/b.bin"), vec![0u8; 100]).unwrap();
        std::fs::write(d.path().join("solo.txt"), "x").unwrap();
        let o = build_manifest(&[d.path().join("proj"), d.path().join("solo.txt")]).unwrap();
        let names: Vec<_> = o.items.iter().map(|i| i.path.as_str()).collect();
        assert_eq!(
            names,
            [
                "proj",
                "proj/a.txt",
                "proj/sub",
                "proj/sub/b.bin",
                "solo.txt"
            ]
        );
        assert_eq!(total_size(&o.items), 106);
    }
}
