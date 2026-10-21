//! Reads live agent state from herdr.
//!
//! v0 shells out to `herdr api snapshot`. v1 should talk to the socket
//! directly and use `events.subscribe` instead of polling.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct Agent {
    /// Agent kind as detected by herdr, e.g. "claude".
    pub agent: String,
    pub agent_status: String,
    pub pane_id: String,
    pub cwd: PathBuf,
    /// Task title herdr extracted from the terminal, if any.
    pub title: Option<String>,
    /// Pane title, which can carry an explicit issue number.
    pub terminal_title: Option<String>,
}

#[derive(Deserialize)]
struct Envelope {
    result: SnapshotResult,
}

#[derive(Deserialize)]
struct SnapshotResult {
    snapshot: Snapshot,
}

#[derive(Deserialize)]
struct Snapshot {
    agents: Vec<Agent>,
}

pub fn parse_snapshot(json: &str) -> Result<Vec<Agent>> {
    let env: Envelope = serde_json::from_str(json).context("parsing herdr snapshot JSON")?;
    Ok(env.result.snapshot.agents)
}

/// Fetches agents from the running herdr server, or from a saved snapshot
/// file when `file` is given (useful for testing without herdr).
pub fn fetch_agents(file: Option<&Path>) -> Result<Vec<Agent>> {
    let json = match file {
        Some(path) => std::fs::read_to_string(path)
            .with_context(|| format!("reading snapshot file {}", path.display()))?,
        None => {
            let out = Command::new("herdr")
                .args(["api", "snapshot"])
                .output()
                .context("running `herdr api snapshot` (is herdr installed?)")?;
            if !out.status.success() {
                bail!(
                    "`herdr api snapshot` failed: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                );
            }
            String::from_utf8(out.stdout).context("herdr output is not UTF-8")?
        }
    };
    parse_snapshot(&json)
}

/// Switches herdr to the agent in `pane`.
pub fn focus_agent(pane: &str) -> Result<()> {
    let out = Command::new("herdr")
        .args(["agent", "focus", pane])
        .output()
        .context("running `herdr agent focus` (is herdr installed?)")?;
    if !out.status.success() {
        bail!(
            "`herdr agent focus` failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_agents_from_a_snapshot_ignoring_unknown_fields() {
        let agents = parse_snapshot(include_str!("../tests/fixtures/snapshot.json")).unwrap();
        assert_eq!(agents.len(), 4);
        assert_eq!(agents[0].title.as_deref(), Some("Set env vars"));
        assert_eq!(
            agents[0].terminal_title.as_deref(),
            Some("Implement #4 | repo")
        );
        assert_eq!(
            agents[2].cwd,
            Path::new("/repo/.claude/worktrees/fix-ranking/src/search")
        );
    }
}
