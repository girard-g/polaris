//! Update-availability check: a tiny cached "is a newer release out?" probe.
//! The network fetch lives in a detached child process; every render path here
//! only reads a local cache file, so it never blocks the caller.

// ponytail: later tasks (network fetch + MCP serve) call every pub item here
#![allow(dead_code)]

use std::sync::atomic::{AtomicBool, Ordering};

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

/// True when the last attempt is older than the TTL.
pub fn is_stale(checked_at: u64, now: u64) -> bool {
    now.saturating_sub(checked_at) >= TTL_SECS
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

/// One-time session banner: returns `"{note}\n\n"` exactly once (first caller
/// that wins the CAS), `""` every time after. `note == None` → always `""`.
pub fn banner_once(note: &Option<String>, shown: &AtomicBool) -> String {
    match note {
        Some(n)
            if shown
                .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok() =>
        {
            format!("{n}\n\n")
        }
        _ => String::new(),
    }
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
                   "Polaris 2.3.0 available — run 'polaris update'.\n\n");
        assert_eq!(banner_once(&note, &shown), ""); // silent thereafter
    }

    #[test]
    fn banner_none_is_empty() {
        let shown = AtomicBool::new(false);
        assert_eq!(banner_once(&None, &shown), "");
    }
}
