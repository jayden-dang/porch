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
