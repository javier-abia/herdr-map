//! Made-up agents and issues for `--demo`, read from a TOML file in place of
//! herdr, GitHub and Jev, so the view can be shown or recorded anywhere.
//!
//! ```toml
//! [[issue]]
//! number = 112
//! title = "Cut token use in the search results"
//! module = "search"      # leave out for an unclassified issue
//! confidence = 0.86      # default 0.9; below min_confidence it is unplaced
//! parent = 100           # optional, another [[issue]]
//!
//! [[agent]]
//! pane = "w1:p1"
//! agent = "claude"       # default "claude"
//! status = ["working", "working", "idle"]  # one per tick, then repeats
//! branch = "search-tokens-112"  # linked to its issue as a real branch is
//! worktree = "search-tokens"    # optional
//! ```
//!
//! An agent without a `branch` is on `main`, so under "No issue".

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::herdr::Agent;
use crate::modulemap::ModuleMap;
use crate::world::{AgentFacts, Issue, IssueRef, RepoFacts, World};
use crate::{graph, tui};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DemoFile {
    #[serde(default, rename = "issue")]
    issues: Vec<DemoIssue>,
    #[serde(default, rename = "agent")]
    agents: Vec<DemoAgent>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DemoIssue {
    number: u64,
    title: String,
    module: Option<String>,
    #[serde(default = "default_confidence")]
    confidence: f32,
    parent: Option<u64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DemoAgent {
    pane: String,
    #[serde(default = "default_agent")]
    agent: String,
    status: Statuses,
    branch: Option<String>,
    worktree: Option<String>,
}

/// One status, or a list stepped through one per tick.
#[derive(Deserialize)]
#[serde(untagged)]
enum Statuses {
    One(String),
    Cycle(Vec<String>),
}

fn default_confidence() -> f32 {
    0.9
}

fn default_agent() -> String {
    "claude".into()
}

const GIT_DIR: &str = "/demo/.git";

pub struct Demo {
    pub world: World,
    /// Each agent's statuses, in `world.agents` order.
    statuses: Vec<Vec<String>>,
    tick: usize,
}

impl Demo {
    pub fn load(path: &Path, map: ModuleMap) -> Result<Demo> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading demo file {}", path.display()))?;
        Demo::parse(&text, map).with_context(|| format!("in demo file {}", path.display()))
    }

    fn parse(text: &str, mut map: ModuleMap) -> Result<Demo> {
        let file: DemoFile = toml::from_str(text)?;

        let titles: BTreeMap<u64, &str> = file
            .issues
            .iter()
            .map(|i| (i.number, i.title.as_str()))
            .collect();
        let mut issues = Vec::new();
        let mut classifications = BTreeMap::new();
        for i in &file.issues {
            if let Some(m) = &i.module {
                if !map.modules.iter().any(|mm| &mm.id == m) {
                    bail!("demo issue #{}: no module {m:?} in the map", i.number);
                }
                classifications.insert(i.number, [(m.clone(), i.confidence)].into());
            }
            let parent = match i.parent {
                Some(n) => Some(IssueRef {
                    number: n,
                    title: titles
                        .get(&n)
                        .with_context(|| format!("demo issue #{}: no parent #{n}", i.number))?
                        .to_string(),
                }),
                None => None,
            };
            issues.push(Issue {
                number: i.number,
                title: i.title.clone(),
                body: String::new(),
                updated_at: String::new(),
                parent,
            });
        }

        // Agents sit in a made-up repo.
        map.root = PathBuf::from("/demo");
        let mut agents = Vec::new();
        let mut statuses = Vec::new();
        for a in file.agents {
            let cycle = match a.status {
                Statuses::One(s) => vec![s],
                Statuses::Cycle(v) if !v.is_empty() => v,
                Statuses::Cycle(_) => bail!("demo agent {}: empty status list", a.pane),
            };
            let toplevel = match &a.worktree {
                Some(w) => PathBuf::from("/demo-worktrees").join(w),
                None => map.root.clone(),
            };
            agents.push(AgentFacts {
                agent: Agent {
                    agent: a.agent,
                    agent_status: cycle[0].clone(),
                    pane_id: a.pane,
                    cwd: toplevel.clone(),
                    title: None,
                    terminal_title: None,
                },
                repo: Some(RepoFacts {
                    git_dir: GIT_DIR.into(),
                    toplevel,
                    on_default_branch: a.branch.is_none(),
                    branch: Some(a.branch.unwrap_or_else(|| "main".into())),
                }),
            });
            statuses.push(cycle);
        }

        Ok(Demo {
            world: World {
                map,
                project_git_dir: Some(GIT_DIR.into()),
                agents,
                issues,
                prs: Vec::new(),
                classifications,
            },
            statuses,
            tick: 0,
        })
    }

    /// Advances every agent to its next status and builds the graph.
    pub fn tick(&mut self) -> tui::Tick {
        for (facts, cycle) in self.world.agents.iter_mut().zip(&self.statuses) {
            facts.agent.agent_status = cycle[self.tick % cycle.len()].clone();
        }
        self.tick += 1;
        Ok((graph::build(&self.world), Vec::new(), Vec::new()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map() -> ModuleMap {
        ModuleMap::parse(
            "[[module]]\nid = \"search\"\n[[module]]\nid = \"read\"\n",
            Path::new("/repo"),
        )
        .unwrap()
    }

    fn load(toml: &str) -> Result<Demo> {
        Demo::parse(toml, map())
    }

    #[test]
    fn places_issues_and_links_agents_through_the_real_graph() {
        let mut demo = load(
            "[[issue]]\nnumber = 7\ntitle = \"Floor\"\nmodule = \"read\"\n\
             [[agent]]\npane = \"p1\"\nstatus = \"working\"\nbranch = \"floor-7\"\n\
             [[agent]]\npane = \"p2\"\nstatus = \"idle\"\n",
        )
        .unwrap();
        let (g, _, _) = demo.tick().unwrap();
        assert_eq!(g.modules[1].issues[0].number, 7);
        assert_eq!(g.modules[1].issues[0].agents[0].agent.pane_id, "p1");
        assert_eq!(g.no_issue_agents[0].agent.pane_id, "p2");
    }

    #[test]
    fn steps_through_status_lists_one_per_tick() {
        let mut demo =
            load("[[agent]]\npane = \"p1\"\nstatus = [\"working\", \"idle\"]\n").unwrap();
        let status = |d: &mut Demo| {
            d.tick().unwrap().0.no_issue_agents[0]
                .agent
                .agent_status
                .clone()
        };
        assert_eq!(status(&mut demo), "working");
        assert_eq!(status(&mut demo), "idle");
        assert_eq!(status(&mut demo), "working");
    }

    #[test]
    fn rejects_an_unknown_module() {
        let e = load("[[issue]]\nnumber = 7\ntitle = \"x\"\nmodule = \"nope\"\n")
            .err()
            .unwrap();
        assert!(e.to_string().contains("no module \"nope\""), "{e}");
    }
}
