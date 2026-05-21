//! `polaris hook` — internal subcommands invoked by Claude Code hooks.
//!
//! Each subcommand reads its hook payload as JSON on stdin and applies its
//! action. All paths exit 0 unconditionally; failures are reported to stderr
//! so a transient hiccup never interrupts the user's session via a Claude Code
//! warning banner.

use polaris_core::error::Result;

/// Entry point for `polaris hook index` — re-index a single file the agent
/// just edited.
pub fn run_index() -> Result<()> {
    // Implementation lands in Task 8–11.
    Ok(())
}
