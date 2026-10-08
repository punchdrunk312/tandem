//! File transfer: manifest building, receive-side sanitising, streaming.

use crate::channel::{SecureReader, SecureWriter};
use crate::error::{Error, Result};
use crate::proto::{FileItem, FileMsg};
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use tokio::fs::File;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const CHUNK: usize = 64 * 1024;
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

/// Sender side of a file connection. Returns the offer id (if the request was
/// read) together with the outcome.
pub async fn serve<R, W>(
    r: &mut SecureReader<R>,
    w: &mut SecureWriter<W>,
    lookup: impl Fn(u64) -> Option<std::sync::Arc<Offer>>,
    progress: &(dyn Fn(u64, u64) + Send + Sync),
) -> (Option<u64>, Result<()>)
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let id = match r.recv::<FileMsg>().await {
        Ok(FileMsg::Request { id }) => id,
        Ok(_) => return (None, Err(Error::Proto("expected Request".into()))),
        Err(e) => return (None, Err(e)),
    };
    (Some(id), serve_offer(w, id, lookup, progress).await)
}

async fn serve_offer<W: AsyncWrite + Unpin>(
    w: &mut SecureWriter<W>,
    id: u64,
    lookup: impl Fn(u64) -> Option<std::sync::Arc<Offer>>,
    progress: &(dyn Fn(u64, u64) + Send + Sync),
) -> Result<()> {
    let Some(offer) = lookup(id) else {
        w.send(&FileMsg::Error("offer expired".into())).await?;
        return Err(Error::Proto("unknown offer".into()));
    };
    let mut sent = 0u64;
    for (i, (item, path)) in offer.items.iter().zip(&offer.paths).enumerate() {
        w.send(&FileMsg::Header { index: i as u32 }).await?;
        if item.is_dir {
            continue;
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
        let mut left = item.size;
        let mut buf = vec![0u8; CHUNK];
        while left > 0 {
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
    // Validate everything and pick collision-free top-level names *before* writing.
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

    w.send(&FileMsg::Request { id }).await?;
    let mut got_total = 0u64;
    for (i, it) in items.iter().enumerate() {
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
        let mut f = File::create(&target).await?;
        let mut got = 0u64;
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
    }
    match r.recv::<FileMsg>().await? {
        FileMsg::Done => {}
        FileMsg::Error(e) => return Err(Error::Rejected(e)),
        _ => return Err(Error::Proto("expected Done".into())),
    }
    Ok(tops.iter().map(|t| dest.join(&renames[t])).collect())
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
