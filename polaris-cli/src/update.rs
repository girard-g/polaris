//! `polaris update` — self-upgrade the binary from GitHub Releases.

use console::style;
use polaris_core::error::{PolarisError, Result};
use self_update::backends::github::Update as GhUpdate;
use self_update::version::bump_is_greater;

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

/// Bare semver of the running binary, without any `v` prefix.
pub fn current_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Entry point for the `update` command.
pub fn run(opts: UpdateOpts) -> Result<()> {
    // 1. Platform check.
    let asset = match target_triple() {
        Some(a) => a,
        None => {
            eprintln!(
                "{}  no release asset for {}-{}.",
                style("✗").red().bold(),
                std::env::consts::OS,
                std::env::consts::ARCH,
            );
            eprintln!(
                "  supported targets: linux-x86_64, macos-aarch64, windows-x86_64"
            );
            eprintln!(
                "  install manually from https://github.com/girard-g/polaris/releases"
            );
            std::process::exit(2);
        }
    };

    let current = current_version();

    // 2. Build the self_update configurator. We share it between the "check" path
    //    and the "install" path; only `target_version_tag` differs.
    let mut builder = GhUpdate::configure();
    builder
        .repo_owner("girard-g")
        .repo_name("polaris")
        .bin_name("polaris")
        .target(asset)
        .current_version(current)
        .show_download_progress(true)
        .no_confirm(true);
    if let Some(v) = &opts.version {
        builder.target_version_tag(&format!("v{v}"));
    }

    // 3. Fetch the target release (latest or pinned) to know what we'd install.
    let updater = builder
        .build()
        .map_err(|e| PolarisError::Update(format!("could not initialise updater: {e}")))?;

    let release = if let Some(v) = &opts.version {
        updater.get_release_version(&format!("v{v}"))
            .map_err(|e| classify_error(e, opts.version.as_deref()))?
    } else {
        updater.get_latest_release()
            .map_err(|e| classify_error(e, None))?
    };

    let target_ver = release.version.trim_start_matches('v').to_string();

    // 4. Compare versions.
    //    bump_is_greater returns Ok(true) iff target_ver > current per semver ordering.
    let is_newer = bump_is_greater(current, &target_ver).unwrap_or(false);
    let is_same = current == target_ver;

    // 5. --check is read-only.
    if opts.check {
        if is_same {
            println!(
                "{}  polaris is up to date  {}",
                style("✓").green().bold(),
                style(format!("(v{current})")).dim(),
            );
        } else if is_newer {
            println!(
                "{}  update available: v{}  →  v{}",
                style("◆").cyan().bold(),
                current,
                target_ver,
            );
        } else {
            // Pinned-version path: target is older than current.
            println!(
                "{}  v{} is older than current v{}",
                style("·").dim(),
                target_ver,
                current,
            );
        }
        return Ok(());
    }

    // 6. No-op short-circuit.
    if is_same && !opts.force {
        println!(
            "{}  already on v{}  {}",
            style("✓").green().bold(),
            current,
            style("(use --force to re-install)").dim(),
        );
        return Ok(());
    }

    // 7. Confirm.
    if !opts.yes {
        print!(
            "Update polaris v{}  →  v{}? [y/N]: ",
            current, target_ver
        );
        // Flush so the prompt appears before we block on stdin.
        let _ = std::io::Write::flush(&mut std::io::stdout());

        let stdin = std::io::stdin();
        let mut stdin_lock = stdin.lock();
        let mut stdout = std::io::stdout();
        let confirmed = prompt_yes_no(&mut stdin_lock, &mut stdout)
            .map_err(|e| PolarisError::Update(format!("could not read confirmation: {e}")))?;
        if !confirmed {
            println!("Cancelled.");
            std::process::exit(1);
        }
    }

    // 8. Install. Reuse `updater` from step 3 — target_version_tag was already set on the
    //    builder before `.build()`, so no second HTTP call is needed.
    let status = updater
        .update()
        .map_err(|e| classify_error(e, opts.version.as_deref()))?;

    println!(
        "{}  polaris updated to v{}",
        style("✓").green().bold(),
        status.version(),
    );
    Ok(())
}

/// Map `self_update::errors::Error` into a friendly `PolarisError::Update(...)`.
///
/// `requested_version` is the value of `--version`, used to produce the
/// "no release tagged vX.Y.Z" message when relevant.
fn classify_error(
    err: self_update::errors::Error,
    requested_version: Option<&str>,
) -> PolarisError {
    use self_update::errors::Error as E;
    let err_str = err.to_string();
    let msg = match (&err, requested_version) {
        // self_update may return E::Release or E::Network(404) for a missing tag.
        (E::Release(m), Some(v)) if m.contains(v) => format!(
            "no release tagged v{v}. See https://github.com/girard-g/polaris/releases for available versions."
        ),
        // GitHub Returns HTTP 404 for unknown tags; self_update wraps this as NetworkError.
        (E::Network(_) | E::Reqwest(_), Some(v))
            if err_str.contains("status: 404") =>
        {
            format!(
                "no release tagged v{v}. See https://github.com/girard-g/polaris/releases for available versions."
            )
        }
        (E::Network(_) | E::Reqwest(_), _) => format!(
            "could not reach github.com ({err}). Check connectivity or try again later."
        ),
        (E::Io(io), _) if matches!(
            io.kind(),
            std::io::ErrorKind::PermissionDenied
        ) => format!(
            "cannot write to the polaris binary ({io}). Re-run with appropriate permissions, or reinstall manually."
        ),
        _ => err.to_string(),
    };
    PolarisError::Update(msg)
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

    #[test]
    fn current_version_is_non_empty() {
        assert!(!current_version().is_empty());
    }

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
