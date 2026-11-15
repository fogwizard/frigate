use std::path::{Path, PathBuf};

/// Resolve relative paths against an install / working root.
pub fn resolve(root: &PathBuf, rel: &str) -> PathBuf {
    let p = PathBuf::from(rel);
    if p.is_absolute() {
        p
    } else {
        root.join(p)
    }
}

/// Prefer `configured` when that file exists.
///
/// A Windows path such as `D:/adb/ffmpeg/bin/ffmpeg.exe` does not exist on
/// macOS or Linux. In that case return the bare tool name (`ffmpeg`) so the
/// process can be found on `PATH` or beside this binary.
pub fn resolve_tool(configured: &str) -> String {
    let configured = configured.trim();
    if configured.is_empty() {
        return String::new();
    }
    let path = Path::new(configured);
    if path.is_file() {
        return configured.to_string();
    }
    let Some(raw) = path.file_name().and_then(|s| s.to_str()) else {
        return configured.to_string();
    };
    #[cfg(windows)]
    {
        let _ = raw;
        configured.to_string()
    }
    #[cfg(not(windows))]
    raw.strip_suffix(".exe").unwrap_or(raw).to_string()
}
