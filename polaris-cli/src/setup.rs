//! `polaris setup` — create/merge .mcp.json and ensure .gitignore entries.

use std::path::Path;

use polaris_core::error::{PolarisError, Result};

/// Entry point for the `setup` command.
pub fn run(_path: &Path) -> Result<()> {
    Err(PolarisError::Setup("not yet implemented".into()))
}
