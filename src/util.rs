//! Small file helpers shared by the operations.

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

pub fn sha256_file(path: &Path) -> Result<String> {
    let mut f = fs::File::open(path).with_context(|| format!("could not open {}", path.display()))?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(hex::encode(h.finalize()))
}

pub fn sha256_bytes(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

/// Recursively copies `src` into `dst` (merging). Returns the copied files relative to `src`.
pub fn copy_dir(src: &Path, dst: &Path) -> Result<Vec<PathBuf>> {
    let mut copied = Vec::new();
    copy_dir_inner(src, dst, Path::new(""), &mut copied)?;
    Ok(copied)
}

fn copy_dir_inner(src: &Path, dst: &Path, rel: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    fs::create_dir_all(dst).with_context(|| format!("could not create {}", dst.display()))?;
    for entry in fs::read_dir(src).with_context(|| format!("could not read {}", src.display()))? {
        let entry = entry?;
        let name = entry.file_name();
        let from = entry.path();
        let to = dst.join(&name);
        let rel_child = rel.join(&name);
        if entry.file_type()?.is_dir() {
            copy_dir_inner(&from, &to, &rel_child, out)?;
        } else {
            fs::copy(&from, &to).with_context(|| format!("could not copy {} to {}", from.display(), to.display()))?;
            out.push(rel_child);
        }
    }
    Ok(())
}

pub fn dir_size(path: &Path) -> u64 {
    let Ok(rd) = fs::read_dir(path) else {
        return fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    };
    rd.flatten()
        .map(|e| match e.file_type() {
            Ok(t) if t.is_dir() => dir_size(&e.path()),
            _ => e.metadata().map(|m| m.len()).unwrap_or(0),
        })
        .sum()
}

pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 { format!("{bytes} B") } else { format!("{v:.1} {}", UNITS[u]) }
}

/// Unpacks a .zip or .7z into `dest` (created fresh).
pub fn extract_archive(archive: &Path, dest: &Path) -> Result<()> {
    if dest.exists() {
        fs::remove_dir_all(dest).ok();
    }
    fs::create_dir_all(dest)?;
    let mut magic = [0u8; 6];
    fs::File::open(archive)
        .with_context(|| format!("could not open {}", archive.display()))?
        .read_exact(&mut magic)
        .context("the file is too small to be an archive")?;
    if magic.starts_with(b"PK") {
        let f = fs::File::open(archive)?;
        let mut zip = zip::ZipArchive::new(f).context("the .zip file is damaged")?;
        // `extract` rejects entries that would escape `dest` (zip-slip).
        zip.extract(dest).context("could not unpack the .zip file")?;
    } else if magic == [b'7', b'z', 0xBC, 0xAF, 0x27, 0x1C] {
        sevenz_rust::decompress_file(archive, dest).map_err(|e| anyhow::anyhow!("could not unpack the .7z file: {e}"))?;
    } else {
        bail!("{} is not a .zip or .7z archive (RAR isn't supported; unpack it yourself and zip the files)", archive.display());
    }
    Ok(())
}

/// Zips the contents of `src` into `dest` (files only; empty folders are skipped).
pub fn zip_dir(src: &Path, dest: &Path, skip: &dyn Fn(&Path) -> bool) -> Result<usize> {
    let f = fs::File::create(dest).with_context(|| format!("could not create {}", dest.display()))?;
    let mut zw = zip::ZipWriter::new(f);
    let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated).large_file(true);
    let mut count = 0;
    let mut stack = vec![src.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();
            let rel = path.strip_prefix(src).unwrap_or(&path);
            if skip(rel) {
                continue;
            }
            if entry.file_type()?.is_dir() {
                stack.push(path);
                continue;
            }
            let name = rel.to_string_lossy().replace('\\', "/");
            zw.start_file(name, opts)?;
            let mut data = Vec::new();
            fs::File::open(&path)?.read_to_end(&mut data)?;
            zw.write_all(&data)?;
            count += 1;
        }
    }
    zw.finish()?;
    Ok(count)
}

/// Returns "major.minor.patch.build" from a PE file's version resource.
pub fn pe_version(path: &Path) -> Option<String> {
    use pelite::FileMap;
    let map = FileMap::open(path).ok()?;
    let fixed = |res: pelite::resources::Resources| -> Option<String> {
        let vi = res.version_info().ok()?;
        let f = vi.fixed()?;
        let v = f.dwFileVersion;
        Some(format!("{}.{}.{}.{}", v.Major, v.Minor, v.Patch, v.Build))
    };
    if let Ok(pe) = pelite::pe64::PeFile::from_bytes(&map) {
        use pelite::pe64::Pe;
        return pe.resources().ok().and_then(fixed);
    }
    if let Ok(pe) = pelite::pe32::PeFile::from_bytes(&map) {
        use pelite::pe32::Pe;
        return pe.resources().ok().and_then(fixed);
    }
    None
}

/// Cheap change detector for a folder: file count, total size, newest mtime.
pub fn fingerprint(dir: &Path) -> String {
    let (mut n, mut size, mut newest) = (0u64, 0u64, 0u64);
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let Ok(m) = e.metadata() else { continue };
            if m.is_dir() {
                stack.push(e.path());
            } else {
                n += 1;
                size += m.len();
                let t = m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs()).unwrap_or(0);
                newest = newest.max(t);
            }
        }
    }
    format!("{n}:{size}:{newest}")
}

/// Hides the user's account/home path in text that may be shared (bug reports, diagnostics).
pub fn redact(text: &str) -> String {
    let mut out = text.to_string();
    if let Some(home) = dirs::home_dir() {
        let h = home.to_string_lossy().to_string();
        if h.len() > 3 {
            out = out.replace(&h, "~");
            out = out.replace(&h.replace('\\', "/"), "~");
        }
    }
    out
}

pub fn open_path(path: &Path) {
    let _ = open::that_detached(path);
}

pub fn open_url(url: &str) {
    let _ = open::that_detached(url);
}

/// Moves a file or folder, falling back to copy+delete across drives.
pub fn move_path(from: &Path, to: &Path) -> Result<()> {
    if let Some(parent) = to.parent() {
        fs::create_dir_all(parent)?;
    }
    if fs::rename(from, to).is_ok() {
        return Ok(());
    }
    if from.is_dir() {
        copy_dir(from, to)?;
        fs::remove_dir_all(from)?;
    } else {
        fs::copy(from, to)?;
        fs::remove_file(from)?;
    }
    Ok(())
}

/// Keeps the newest `keep` entries (by name, which starts with a timestamp) in `dir`.
pub fn prune_dir(dir: &Path, keep: usize, protect: Option<&Path>) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    let mut entries: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
    entries.sort();
    while entries.len() > keep {
        let old = entries.remove(0);
        if Some(old.as_path()) == protect {
            continue;
        }
        if old.is_dir() {
            let _ = fs::remove_dir_all(&old);
        } else {
            let _ = fs::remove_file(&old);
        }
    }
}

/// Forward-slash relative path string for state files.
pub fn rel_string(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(human_size(10), "10 B");
        assert_eq!(human_size(2048), "2.0 KB");
    }

    #[test]
    fn zip_roundtrip() {
        let base = std::env::temp_dir().join(format!("bl-zip-{}", std::process::id()));
        let src = base.join("src");
        fs::create_dir_all(src.join("a")).unwrap();
        fs::write(src.join("a/b.txt"), "hi").unwrap();
        fs::write(src.join("skip.log"), "x").unwrap();
        let z = base.join("t.zip");
        let n = zip_dir(&src, &z, &|p| p.to_string_lossy().ends_with(".log")).unwrap();
        assert_eq!(n, 1);
        let out = base.join("out");
        extract_archive(&z, &out).unwrap();
        assert_eq!(fs::read_to_string(out.join("a/b.txt")).unwrap(), "hi");
        fs::remove_dir_all(&base).ok();
    }
}
