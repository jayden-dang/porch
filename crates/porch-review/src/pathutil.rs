//! PATH / executable helpers for setup + `review_bin` resolution.

use std::env;
use std::path::{Path, PathBuf};

/// Resolve `name_or_path` as an absolute path or a PATH lookup.
#[must_use]
pub fn resolve_bin(name_or_path: &str) -> Option<PathBuf> {
    let p = Path::new(name_or_path);
    if p.is_absolute() || name_or_path.contains('/') || name_or_path.contains('\\') {
        return is_executable(p).then(|| p.to_path_buf());
    }
    which(name_or_path)
}

/// First executable named `name` on `PATH`.
#[must_use]
pub fn which(name: &str) -> Option<PathBuf> {
    let path = env::var_os("PATH")?;
    for dir in env::split_paths(&path) {
        let candidate = dir.join(name);
        if is_executable(&candidate) {
            return Some(candidate);
        }
    }
    None
}

/// True when `path` exists and is executable (Unix mode bit; existence elsewhere).
#[must_use]
pub fn is_executable(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).is_ok_and(|m| m.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// Set executable bits `0755` on Unix.
///
/// # Errors
///
/// Returns I/O errors from `set_permissions`.
pub fn chmod_755(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(path)?.permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(path, perms)?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}
