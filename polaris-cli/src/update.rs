//! `polaris update` — self-upgrade the binary from GitHub Releases.

use polaris_core::error::{PolarisError, Result};

/// Options parsed from the `polaris update` subcommand.
#[derive(Debug, Clone)]
pub struct UpdateOpts {
    /// Read-only: print latest vs current and exit.
    pub check: bool,
    /// Skip the confirmation prompt.
    pub yes: bool,
    /// Install a specific version (pin or downgrade). Bare version, no `v` prefix.
    pub version: Option<String>,
    /// Re-install even when already on the target version.
    pub force: bool,
}

/// Entry point for the `update` command.
pub fn run(_opts: UpdateOpts) -> Result<()> {
    Err(PolarisError::Update("not yet implemented".into()))
}
