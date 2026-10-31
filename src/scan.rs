use std::{
    fs,
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

pub const ROOT: &str = "/storage/emulated/0";
pub const TRASH: &str = "/storage/emulated/0/.photoswipe_trash";

#[derive(Clone, Debug)]
pub struct Photo {
    pub path: PathBuf,
    pub mtime: u64,
}

fn is_image(p: &Path) -> bool {
    match p.extension().and_then(|e| e.to_str()) {
        Some(e) => {
            let e = e.to_ascii_lowercase();
            matches!(e.as_str(), "jpg" | "jpeg" | "png" | "webp" | "gif" | "bmp")
        }
        None => false,
    }
}

pub fn mtime_of(p: &Path) -> u64 {
    fs::metadata(p)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// يلف على التخزين الداخلي ويرجع كل الصور (الاحدث الاول)
pub fn scan() -> Vec<Photo> {
    let mut out: Vec<Photo> = Vec::new();
    let mut stack: Vec<PathBuf> = vec![PathBuf::from(ROOT)];
    while let Some(dir) = stack.pop() {
        let rd = match fs::read_dir(&dir) {
            Ok(r) => r,
            Err(_) => continue,
        };
        let entries: Vec<fs::DirEntry> = rd.flatten().collect();
        if entries
            .iter()
            .any(|e| e.file_name().to_str() == Some(".nomedia"))
        {
            continue;
        }
        let is_root = dir.as_path() == Path::new(ROOT);
        for e in entries {
            let path = e.path();
            let name = e.file_name().to_string_lossy().into_owned();
            let ft = match e.file_type() {
                Ok(t) => t,
                Err(_) => continue,
            };
            if ft.is_dir() {
                if name.starts_with('.') {
                    continue;
                }
                if is_root && name == "Android" {
                    stack.push(path.join("media"));
                    continue;
                }
                stack.push(path);
            } else if ft.is_file() && is_image(&path) {
                if let Ok(md) = e.metadata() {
                    if md.len() < 5_000 {
                        continue;
                    }
                    let mtime = md
                        .modified()
                        .ok()
                        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                        .map(|d| d.as_secs())
                        .unwrap_or(0);
                    out.push(Photo { path, mtime });
                }
            }
        }
    }
    out.sort_by(|a, b| b.mtime.cmp(&a.mtime).then_with(|| a.path.cmp(&b.path)));
    out
}
