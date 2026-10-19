use std::path::PathBuf;

/// Resolve relative paths against an install / working root.
pub fn resolve(root: &PathBuf, rel: &str) -> PathBuf {
    let p = PathBuf::from(rel);
    if p.is_absolute() {
        p
    } else {
        root.join(p)
    }
}
