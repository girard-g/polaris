//! Update-availability check: a tiny cached "is a newer release out?" probe.
//! The network fetch lives in a detached child process; every render path here
//! only reads a local cache file, so it never blocks the caller.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use self_update::backends::github::Update as GhUpdate;
use serde::{Deserialize, Serialize};

/// Re-check GitHub at most this often (seconds).
const TTL_SECS: u64 = 86_400;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheFile {
    /// Latest release version seen, without any `v` prefix.
    pub latest: String,
    /// Unix seconds of the last refresh *attempt* (success or failure).
    pub checked_at: u64,
}

/// Parse the on-disk cache. Any error (missing field, bad JSON) → `None`.
pub fn parse_cache(s: &str) -> Option<CacheFile> {
    serde_json::from_str(s).ok()
}

/// True when the last attempt is older than the TTL. A `checked_at` in the
/// future (wall clock stepped backward) is also stale, so a backward clock
/// jump can't freeze the cache as "fresh forever".
pub fn is_stale(checked_at: u64, now: u64) -> bool {
    now < checked_at || now - checked_at >= TTL_SECS
}

/// True when there is no cache, or the cache is stale.
pub fn should_refresh(cache: &Option<CacheFile>, now: u64) -> bool {
    match cache {
        None => true,
        Some(c) => is_stale(c.checked_at, now),
    }
}

/// `Some(latest)` iff the cached latest version is strictly newer than `current`.
pub fn pending_from(current: &str, cache: &Option<CacheFile>) -> Option<String> {
    let latest = &cache.as_ref()?.latest;
    match self_update::version::bump_is_greater(current, latest) {
        Ok(true) => Some(latest.clone()),
        _ => None,
    }
}

/// Build the cache row to write after a refresh attempt. On a failed fetch
/// (`fetched == None`) keep the previous `latest` so we never erase a known
/// update; always advance `checked_at` so failures still back off a full TTL.
fn merge_refresh(fetched: Option<String>, prev: &Option<CacheFile>, now: u64) -> CacheFile {
    let latest = fetched
        .or_else(|| prev.as_ref().map(|c| c.latest.clone()))
        .unwrap_or_default();
    CacheFile { latest, checked_at: now }
}

/// One-time session banner: returns `"\n\n{note}"` exactly once (first caller
/// that wins the CAS), `""` every time after. `note == None` → always `""`.
/// It's a *suffix* (appended after the tool output) so an error response still
/// begins with `Error:` — the only failure signal, since rmcp returns these as
/// `isError: false`.
pub fn banner_once(note: &Option<String>, shown: &AtomicBool) -> String {
    match note {
        Some(n)
            if shown
                .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok() =>
        {
            format!("\n\n{n}")
        }
        _ => String::new(),
    }
}

fn cache_path() -> Option<PathBuf> {
    // `dirs::cache_dir()` (already used by polaris-core) resolves the platform
    // cache root and filters empty/relative XDG values — and unlike a hand-rolled
    // `$HOME/.cache`, it returns %LOCALAPPDATA% on Windows and ~/Library/Caches
    // on macOS, both supported release targets.
    Some(dirs::cache_dir()?.join("polaris").join("update-check.json"))
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn read_cache() -> Option<CacheFile> {
    let path = cache_path()?;
    let raw = std::fs::read_to_string(path).ok()?;
    parse_cache(&raw)
}

fn write_cache(c: &CacheFile) {
    let Some(path) = cache_path() else { return };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string(c) {
        // Atomic replace: write a temp sibling then rename, so a concurrent
        // reader never observes a truncated file (rename is atomic on POSIX).
        let tmp = path.with_extension("json.tmp");
        if std::fs::write(&tmp, json).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }
}

/// Blocking GitHub Releases call. `None` on unsupported platform or any error.
fn fetch_latest() -> Option<String> {
    let asset = crate::update::target_triple()?;
    let updater = GhUpdate::configure()
        .repo_owner("girard-g")
        .repo_name("polaris")
        .bin_name("polaris")
        .target(asset)
        .current_version(crate::update::current_version())
        .build()
        .ok()?;
    let release = updater.get_latest_release().ok()?;
    Some(release.version.trim_start_matches('v').to_string())
}

/// Entry point for the hidden `update-refresh` child process.
pub fn run_refresh() {
    let prev = read_cache();
    let row = merge_refresh(fetch_latest(), &prev, now_unix());
    write_cache(&row);
}

/// True when env var `name` is set to a truthy value. Treats unset and the
/// usual falsey strings (empty/`0`/`false`/`no`) as off, so `CI=false` or
/// `POLARIS_NO_UPDATE_CHECK=0` does NOT disable checks.
fn env_flag(name: &str) -> bool {
    std::env::var(name)
        .map(|v| !matches!(v.trim().to_ascii_lowercase().as_str(), "" | "0" | "false" | "no"))
        .unwrap_or(false)
}

/// True when the user has explicitly opted out of update checks (env var) or
/// we're in CI. Gates the network fetch and every notice surface — NOT the
/// display-only suppressions (hook/json/serve/watch), which still warm the cache.
pub fn check_disabled() -> bool {
    env_flag("POLARIS_NO_UPDATE_CHECK") || env_flag("CI")
}

/// Spawn the detached `update-refresh` child (the caller has already decided
/// the cache is stale). Reaps it on a short-lived thread so a long-lived
/// `serve` parent doesn't accumulate a zombie, and bounds its life to ~30s so a
/// stalled GitHub connection can't linger forever (self_update's blocking
/// client has no timeout). Short CLI parents exit first and init reaps.
fn spawn_refresh() {
    let Ok(exe) = std::env::current_exe() else { return };
    // ponytail: one 24h-stale moment with N concurrent commands can spawn N
    // children before any stamps checked_at; harmless (rare, idempotent write).
    // Add a lockfile only if it ever matters.
    let child = std::process::Command::new(exe)
        .arg("update-refresh")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    // ponytail: a reaper thread is the smallest fix for both the zombie and the
    // no-timeout hang; ~30s ceiling.
    if let Ok(mut child) = child {
        std::thread::spawn(move || {
            for _ in 0..60 {
                match child.try_wait() {
                    Ok(None) => std::thread::sleep(std::time::Duration::from_millis(500)),
                    _ => return, // exited or errored — stop polling
                }
            }
            let _ = child.kill();
            let _ = child.wait();
        });
    }
}

/// Warm the cache (spawn a detached refresh if it's stale) and return the
/// pending update version, if any — reading the cache exactly once. Returns
/// `None` and does nothing when checks are disabled (opt-out env / CI), so
/// every notice surface inherits the gate for free.
pub fn refresh_and_pending() -> Option<String> {
    if check_disabled() {
        return None;
    }
    let cache = read_cache();
    // Skip the refresh child when there's no cache dir to write to — otherwise a
    // sandbox with no resolvable cache root re-spawns a doomed child every call.
    if cache_path().is_some() && should_refresh(&cache, now_unix()) {
        spawn_refresh();
    }
    pending_from(crate::update::current_version(), &cache)
}

/// Should the human-facing stderr notice be withheld for this invocation?
/// `is_long_running` covers `serve`/`watch` (they block, and the MCP path has
/// its own notice); `is_hook` covers agent-fed hook output; `is_json` covers
/// machine-readable command output; `disabled` is [`check_disabled`] (opt-out
/// env / CI), the single source of truth for the env gate.
pub fn suppressed(is_hook: bool, is_long_running: bool, is_json: bool, disabled: bool) -> bool {
    is_hook || is_long_running || is_json || disabled
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cache(latest: &str, checked_at: u64) -> Option<CacheFile> {
        Some(CacheFile { latest: latest.to_string(), checked_at })
    }

    #[test]
    fn parse_valid() {
        let c = parse_cache(r#"{"latest":"2.3.0","checked_at":1751299200}"#).unwrap();
        assert_eq!(c.latest, "2.3.0");
        assert_eq!(c.checked_at, 1_751_299_200);
    }

    #[test]
    fn parse_corrupt_is_none() {
        assert!(parse_cache("not json").is_none());
        assert!(parse_cache(r#"{"latest":"2.3.0"}"#).is_none()); // missing checked_at
        assert!(parse_cache("").is_none());
    }

    #[test]
    fn staleness() {
        assert!(!is_stale(1_000, 1_000 + TTL_SECS - 1)); // within window
        assert!(is_stale(1_000, 1_000 + TTL_SECS + 1));  // past window
        assert!(is_stale(2_000, 1_000));                 // clock stepped backward → stale
    }

    #[test]
    fn should_refresh_cases() {
        assert!(should_refresh(&None, 5_000)); // no cache → refresh
        assert!(should_refresh(&cache("2.3.0", 0), 0 + TTL_SECS + 1)); // stale
        assert!(!should_refresh(&cache("2.3.0", 5_000), 5_000)); // fresh
    }

    #[test]
    fn pending_detects_newer() {
        assert_eq!(pending_from("2.2.3", &cache("2.3.0", 0)).as_deref(), Some("2.3.0"));
        assert_eq!(pending_from("2.3.0", &cache("2.3.0", 0)), None); // equal
        assert_eq!(pending_from("2.4.0", &cache("2.3.0", 0)), None); // older
        assert_eq!(pending_from("2.2.3", &None), None);             // no cache
    }

    #[test]
    fn banner_fires_once() {
        let note = Some("Polaris 2.3.0 available — run 'polaris update'.".to_string());
        let shown = AtomicBool::new(false);
        assert_eq!(banner_once(&note, &shown),
                   "\n\nPolaris 2.3.0 available — run 'polaris update'.");
        assert_eq!(banner_once(&note, &shown), ""); // silent thereafter
    }

    #[test]
    fn banner_none_is_empty() {
        let shown = AtomicBool::new(false);
        assert_eq!(banner_once(&None, &shown), "");
    }

    #[test]
    fn merge_keeps_latest_on_failed_fetch() {
        let prev = cache("2.3.0", 100);
        let row = merge_refresh(None, &prev, 9_999);
        assert_eq!(row.latest, "2.3.0");   // preserved
        assert_eq!(row.checked_at, 9_999); // still stamped → backs off
    }

    #[test]
    fn merge_takes_new_version_on_success() {
        let prev = cache("2.3.0", 100);
        let row = merge_refresh(Some("2.4.0".to_string()), &prev, 9_999);
        assert_eq!(row.latest, "2.4.0");
        assert_eq!(row.checked_at, 9_999);
    }

    #[test]
    fn merge_empty_when_no_data() {
        let row = merge_refresh(None, &None, 42);
        assert_eq!(row.latest, "");
        assert_eq!(row.checked_at, 42);
    }

    #[test]
    fn suppressed_rules() {
        // plain interactive command → show
        assert!(!suppressed(false, false, false, false));
        // each independent reason suppresses
        assert!(suppressed(true, false, false, false));  // hook
        assert!(suppressed(false, true, false, false));  // serve/watch
        assert!(suppressed(false, false, true, false));  // --output json
        assert!(suppressed(false, false, false, true));  // disabled (opt-out / CI)
    }
}
