use std::io::Read;

use crate::Result;

/// M1: dead gate. Consume pre-receive lines and allow the update.
///
/// # Errors
///
/// Returns [`crate::Error::Io`] if stdin cannot be read.
pub fn admit_push(mut stdin: impl Read) -> Result<()> {
    let mut buf = String::new();
    stdin.read_to_string(&mut buf)?;
    tracing::debug!(lines = buf.lines().count(), "admit-push allowed");
    Ok(())
}
