mod cache;
mod demo;
mod git;
mod github;
mod graph;
mod herdr;
mod jev;
mod modulemap;
mod render;
mod tui;
mod world;

use std::collections::BTreeMap;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;

use cache::Cache;
use github::GitHub;
use jev::Jev;
use modulemap::ModuleMap;
use world::{AgentFacts, Classification, World};

/// Shows which modules and GitHub issues your herdr-managed agents are working on.
#[derive(Parser)]
#[command(
    version,
    after_help = "Are you an AI asked to write or change a module map (.herdr-map.toml)?\n  Run: herdr-map --skill"
)]
struct Args {
    /// Module map to use. Defaults to the nearest `.herdr-map.toml`
    /// found walking up from the current directory.
    #[arg(long)]
    map: Option<PathBuf>,

    /// Project root; overrides `project.root` from the map.
    #[arg(long)]
    root: Option<PathBuf>,

    /// Print the map once and exit instead of opening the interactive view.
    /// Implied when stdout is not a terminal.
    #[arg(short, long)]
    once: bool,

    /// Seconds between updates in the interactive view.
    #[arg(long, default_value_t = 2.0)]
    interval: f64,

    /// How the interactive view places modules at the start; `v` switches it.
    #[arg(long, value_enum, default_value_t)]
    layout: tui::Placement,

    /// How the interactive view draws arrows that skip a row at the start; `e`
    /// switches it.
    #[arg(long, value_enum, default_value_t)]
    arrows: tui::Arrows,

    /// Read agents from a saved `herdr api snapshot` JSON file instead of herdr.
    #[arg(long)]
    snapshot: Option<PathBuf>,

    /// Show the made-up agents and issues in this TOML file instead of
    /// reading herdr, GitHub and Jev (see examples/agentic-rag.demo.toml).
    #[arg(long, conflicts_with_all = ["root", "snapshot", "no_github"])]
    demo: Option<PathBuf>,

    /// Seconds to cache GitHub issues and PRs between refreshes.
    #[arg(long, default_value_t = 60)]
    github_ttl: u64,

    /// Don't query GitHub; agents are then all listed under "No issue".
    #[arg(long)]
    no_github: bool,

    /// Disable ANSI colors when printing once (default when stdout is not a
    /// terminal).
    #[arg(long)]
    no_color: bool,

    /// Print the agent skill for writing a module map and exit.
    #[arg(long)]
    skill: bool,
}

/// Bundled so the instructions always match this binary's map format.
const SKILL: &str = include_str!("../skills/herdr-map-skill/SKILL.md");

fn main() -> Result<()> {
    let args = Args::parse();
    if args.skill {
        print!("{SKILL}");
        return Ok(());
    }

    let map_path = match args.map {
        Some(p) => p,
        None => {
            let cwd = std::env::current_dir()?;
            modulemap::discover(&cwd).with_context(|| {
                format!(
                    "no {} found in {} or its parents (pass --map)",
                    modulemap::FILE_NAME,
                    cwd.display()
                )
            })?
        }
    };
    let mut map = ModuleMap::load(&map_path)?;
    let once = args.once || !std::io::stdout().is_terminal();
    let modes = tui::Modes {
        placement: args.layout,
        arrows: args.arrows,
    };
    let interval = Duration::from_secs_f64(args.interval);

    if let Some(path) = args.demo {
        let mut demo = demo::Demo::load(&path, map)?;
        if once {
            let (graph, _, _) = demo.tick()?;
            let style = render::Style {
                color: !args.no_color && std::io::stdout().is_terminal(),
            };
            print!(
                "{}",
                render::render(&demo.world.map, &graph, &[], &[], &style)
            );
            return Ok(());
        }
        let map = tui::MapInfo::from(&demo.world.map);
        return tui::run(map, modes, move || (demo.tick(), interval));
    }

    if let Some(root) = args.root {
        map.root = root;
    }
    // herdr reports absolute, resolved cwds; compare against the same form.
    map.root = map
        .root
        .canonicalize()
        .with_context(|| format!("project root {} does not exist", map.root.display()))?;

    let github = GitHub::new(&map.root, Duration::from_secs(args.github_ttl));
    let world = World {
        project_git_dir: git::repo_facts(&map.root).map(|r| r.git_dir),
        map,
        agents: Vec::new(),
        issues: Vec::new(),
        prs: Vec::new(),
        classifications: Default::default(),
    };
    let cache_path = cache::path(&world.map.name);
    let mut collectors = Collectors {
        cache: cache_path.as_deref().map(Cache::load).unwrap_or_default(),
        cache_path,
        world,
        github: (!args.no_github).then_some(github),
        jev: Jev::from_env(),
        snapshot: args.snapshot,
    };

    if once {
        let (graph, errors, warnings) = collectors.tick()?;
        let style = render::Style {
            color: !args.no_color && std::io::stdout().is_terminal(),
        };
        print!(
            "{}",
            render::render(&collectors.world.map, &graph, &errors, &warnings, &style)
        );
        return Ok(());
    }

    let map = tui::MapInfo::from(&collectors.world.map);
    tui::run(map, modes, move || (collectors.tick(), interval))
}

/// Everything a tick reads from, and the World it fills.
struct Collectors {
    world: World,
    /// None with `--no-github`.
    github: Option<GitHub>,
    jev: Jev,
    cache: Cache,
    cache_path: Option<PathBuf>,
    /// A saved herdr snapshot to read instead of herdr.
    snapshot: Option<PathBuf>,
}

impl Collectors {
    /// One collection pass: reads herdr and GitHub into the World, classifies
    /// the issues agents are on, and builds the graph. Errors are failures
    /// worth flagging on top (the classifier or its cache); warnings are
    /// sources serving stale data.
    fn tick(&mut self) -> tui::Tick {
        let world = &mut self.world;
        world.agents = herdr::fetch_agents(self.snapshot.as_deref())?
            .into_iter()
            .map(|agent| AgentFacts {
                repo: git::repo_facts(&agent.cwd),
                agent,
            })
            .collect();
        let mut warnings = Vec::new();
        if let Some(github) = &mut self.github {
            let data = github.data();
            (world.issues, world.prs) = (data.issues, data.prs);
            if let Some(e) = &github.error {
                warnings.push(format!("GitHub unavailable, showing cached issues: {e}"));
            }
        }

        let mut errors = Vec::new();
        world.classifications = classify(
            world,
            &self.jev,
            &mut self.cache,
            self.cache_path.as_deref(),
            &mut errors,
        );

        Ok((graph::build(world), errors, warnings))
    }
}

/// Classifies the issues live agents are on: cached classifications first,
/// Jev only for the misses, and the fresh results saved for the next tick.
/// With nothing to classify, Jev is not asked and no error is shown.
fn classify(
    world: &World,
    jev: &Jev,
    cache: &mut Cache,
    cache_path: Option<&Path>,
    errors: &mut Vec<String>,
) -> BTreeMap<u64, Classification> {
    let modules = &world.map.modules;
    let mut classifications = BTreeMap::new();
    let wanted = graph::issues_to_classify(world);
    if wanted.is_empty() {
        return classifications;
    }
    let mut misses = Vec::new();
    for issue in wanted {
        match cache.get(&cache::key(issue, modules, jev::MODEL)) {
            Some(c) => {
                classifications.insert(issue.number, c.clone());
            }
            None => misses.push(issue),
        }
    }

    let classified = jev.classify(&misses, modules);
    errors.extend(classified.failure.map(|f| f.to_string()));
    let fresh = classified.classifications;
    if fresh.is_empty() {
        return classifications;
    }
    for issue in misses {
        if let Some(c) = fresh.get(&issue.number) {
            cache.insert(cache::key(issue, modules, jev::MODEL), c.clone());
        }
    }
    if let Some(path) = cache_path
        && let Err(e) = cache.save(path)
    {
        errors.push(format!("classification cache not saved: {e:#}"));
    }
    classifications.extend(fresh);
    classifications
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::herdr::Agent;
    use crate::world::{Issue, RepoFacts};

    const GIT_DIR: &str = "/home/u/urbandocs/.git";

    fn world(agents: Vec<AgentFacts>) -> World {
        World {
            map: ModuleMap::parse(
                "[[module]]\nid = \"search\"\n",
                Path::new("/home/u/urbandocs"),
            )
            .unwrap(),
            project_git_dir: Some(GIT_DIR.into()),
            agents,
            issues: vec![Issue {
                number: 102,
                title: "Floor search results".into(),
                body: String::new(),
                updated_at: "2026-10-01T10:00:00Z".into(),
                parent: None,
            }],
            prs: Vec::new(),
            classifications: Default::default(),
        }
    }

    fn agent_on(branch: &str) -> AgentFacts {
        AgentFacts {
            agent: Agent {
                agent: "claude".into(),
                agent_status: "working".into(),
                pane_id: "p1".into(),
                cwd: "/home/u/urbandocs".into(),
                title: None,
                terminal_title: None,
            },
            repo: Some(RepoFacts {
                git_dir: GIT_DIR.into(),
                toplevel: "/home/u/urbandocs".into(),
                branch: Some(branch.into()),
                on_default_branch: false,
            }),
        }
    }

    #[test]
    fn no_key_error_when_nothing_needs_classifying() {
        let w = world(vec![]);
        let mut errors = Vec::new();
        let c = classify(
            &w,
            &Jev::without_key(),
            &mut Cache::default(),
            None,
            &mut errors,
        );
        assert!(c.is_empty());
        assert!(errors.is_empty(), "{errors:?}");
    }

    #[test]
    fn no_key_error_shown_even_when_every_issue_is_cached() {
        let w = world(vec![agent_on("search-floor-102")]);
        let mut cache = Cache::default();
        let cached: Classification = [("search".to_string(), 0.8)].into();
        cache.insert(
            cache::key(&w.issues[0], &w.map.modules, jev::MODEL),
            cached.clone(),
        );
        let mut errors = Vec::new();
        let c = classify(&w, &Jev::without_key(), &mut cache, None, &mut errors);
        assert_eq!(c.get(&102), Some(&cached));
        assert_eq!(errors, ["HERDR_MAP_OPENROUTER_API_KEY not set"]);
    }
}
