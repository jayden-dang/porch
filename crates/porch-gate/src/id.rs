use std::path::Path;

use sha2::{Digest, Sha256};

/// First 12 hex of sha256(absolute working path).
#[must_use]
pub fn repo_id_for(work_tree: &Path) -> String {
    let abs = work_tree
        .canonicalize()
        .unwrap_or_else(|_| work_tree.to_path_buf());
    let mut hasher = Sha256::new();
    hasher.update(abs.to_string_lossy().as_bytes());
    let digest = hasher.finalize();
    hex::encode(&digest[..6])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_id_for_stable_on_same_absolute_path() {
        let dir = std::env::temp_dir().join(format!("porch-repo-id-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let a = repo_id_for(&dir);
        let b = repo_id_for(&dir.canonicalize().unwrap());
        assert_eq!(a, b);
        assert_eq!(a.len(), 12);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
