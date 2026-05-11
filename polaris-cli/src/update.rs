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

/// Maps the running platform to the asset name produced by `.github/workflows/release.yml`.
///
/// Returns `None` when the running platform has no release asset; callers must emit a
/// friendly error and exit with code 2 in that case.
pub fn target_triple() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => Some("polaris-linux-x86_64"),
        ("macos", "aarch64") => Some("polaris-macos-aarch64"),
        ("windows", "x86_64") => Some("polaris-windows-x86_64.exe"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    #[test]
    fn target_triple_linux_x86_64() {
        assert_eq!(target_triple(), Some("polaris-linux-x86_64"));
    }

    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    #[test]
    fn target_triple_macos_aarch64() {
        assert_eq!(target_triple(), Some("polaris-macos-aarch64"));
    }

    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    #[test]
    fn target_triple_windows_x86_64() {
        assert_eq!(target_triple(), Some("polaris-windows-x86_64.exe"));
    }

    /// Always-on smoke test: the function must not panic on any host.
    #[test]
    fn target_triple_does_not_panic() {
        let _ = target_triple();
    }
}
