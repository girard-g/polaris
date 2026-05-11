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

/// Read a single line from `reader` and treat it as a yes/no answer.
///
/// Accepts `y`, `yes`, `Y`, `YES` (case-insensitive) as yes. Anything else — including
/// EOF, an empty line, or `maybe` — counts as no. The prompt text must be written by
/// the caller before invoking this function.
///
/// `writer` is currently unused but kept so callers can route the answer echo if
/// desired in the future; tests pass a `Vec<u8>` sink.
pub fn prompt_yes_no(
    reader: &mut impl std::io::BufRead,
    _writer: &mut impl std::io::Write,
) -> std::io::Result<bool> {
    let mut line = String::new();
    let n = reader.read_line(&mut line)?;
    if n == 0 {
        // EOF before any data — treat as "no".
        return Ok(false);
    }
    let answer = line.trim().to_ascii_lowercase();
    Ok(matches!(answer.as_str(), "y" | "yes"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn yn(input: &str) -> bool {
        let mut reader = Cursor::new(input.as_bytes().to_vec());
        let mut writer: Vec<u8> = Vec::new();
        prompt_yes_no(&mut reader, &mut writer).unwrap()
    }

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

    #[test]
    fn prompt_yes_no_accepts_y_lower() { assert!(yn("y\n")); }
    #[test]
    fn prompt_yes_no_accepts_yes_lower() { assert!(yn("yes\n")); }
    #[test]
    fn prompt_yes_no_accepts_y_upper() { assert!(yn("Y\n")); }
    #[test]
    fn prompt_yes_no_accepts_yes_upper() { assert!(yn("YES\n")); }
    #[test]
    fn prompt_yes_no_accepts_yes_mixed() { assert!(yn("Yes\n")); }
    #[test]
    fn prompt_yes_no_rejects_n() { assert!(!yn("n\n")); }
    #[test]
    fn prompt_yes_no_rejects_empty_line() { assert!(!yn("\n")); }
    #[test]
    fn prompt_yes_no_rejects_eof() { assert!(!yn("")); }
    #[test]
    fn prompt_yes_no_rejects_garbage() { assert!(!yn("maybe\n")); }
    #[test]
    fn prompt_yes_no_trims_trailing_whitespace() { assert!(yn("  y  \n")); }
}
