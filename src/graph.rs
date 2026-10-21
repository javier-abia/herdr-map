//! The core: turns a world snapshot into the module → issue → agent graph.
//! Pure — all I/O lives in the collectors.

use std::collections::BTreeMap;

use crate::herdr::Agent;
use crate::modulemap::ModuleMap;
use crate::world::{AgentFacts, Classification, Issue, IssueRef, PullRequest, World};

/// Where a link came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkSource {
    BranchNumber,
    TitleMatch,
    PullRequest,
    AgentTitle,
    /// Issue → module, from the classifier; confidence is its probability.
    Classifier,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Link<T> {
    pub target: T,
    pub source: LinkSource,
    /// How sure we are the link is right; 1.0 for deterministic sources.
    pub confidence: f32,
}

impl<T> Link<T> {
    fn certain(target: T, source: LinkSource) -> Self {
        Self {
            target,
            source,
            confidence: 1.0,
        }
    }
}

#[derive(Debug, Clone)]
pub struct AgentNode {
    pub agent: Agent,
    pub branch: Option<String>,
    /// Worktree name when the agent is not in the project's main checkout.
    pub worktree: Option<String>,
    pub issue: Option<Link<u64>>,
}

#[derive(Debug, Clone)]
pub struct IssueNode {
    pub number: u64,
    pub title: String,
    pub parent: Option<IssueRef>,
    /// The issue's most likely module (an index into `ModuleMap::modules`).
    /// For a placed issue it is the module it sits under; for an unplaced one
    /// it is the best guess, below `min_confidence`. None when unclassified.
    pub module: Option<Link<usize>>,
    /// Never empty: only issues a live agent is on are shown.
    pub agents: Vec<AgentNode>,
}

#[derive(Debug, Default)]
pub struct ModuleNode {
    pub issues: Vec<IssueNode>,
}

#[derive(Debug, Default)]
pub struct Graph {
    /// Indexed like `ModuleMap::modules`.
    pub modules: Vec<ModuleNode>,
    /// Issues with a live agent that are not placed in any module.
    pub unplaced_issues: Vec<IssueNode>,
    /// Project agents with no issue inferred from their branch or title.
    pub no_issue_agents: Vec<AgentNode>,
    /// Agents working in other projects.
    pub elsewhere: usize,
}

/// The live agents sorted by what they are on: an issue, no issue, or
/// another project.
#[derive(Default)]
struct LinkedAgents {
    by_issue: BTreeMap<u64, Vec<AgentNode>>,
    no_issue: Vec<AgentNode>,
    elsewhere: usize,
}

fn link_agents(world: &World) -> LinkedAgents {
    let mut linked = LinkedAgents::default();
    for facts in &world.agents {
        if !in_project(world, facts) {
            linked.elsewhere += 1;
            continue;
        }
        let node = agent_node(world, facts);
        match &node.issue {
            Some(link) => linked.by_issue.entry(link.target).or_default().push(node),
            None => linked.no_issue.push(node),
        }
    }
    linked
}

pub fn build(world: &World) -> Graph {
    let map = &world.map;
    let LinkedAgents {
        mut by_issue,
        no_issue,
        elsewhere,
    } = link_agents(world);
    let mut graph = Graph {
        modules: (0..map.modules.len())
            .map(|_| ModuleNode::default())
            .collect(),
        no_issue_agents: no_issue,
        elsewhere,
        ..Default::default()
    };

    // Only issues a live agent is on are shown.
    for issue in &world.issues {
        let Some(agents) = by_issue.remove(&issue.number) else {
            continue;
        };
        let module = world
            .classifications
            .get(&issue.number)
            .and_then(|c| best_module(map, c));
        let node = IssueNode {
            number: issue.number,
            title: issue.title.clone(),
            parent: issue.parent.clone(),
            module,
            agents,
        };
        match &node.module {
            Some(link) if link.confidence >= map.min_confidence => {
                graph.modules[link.target].issues.push(node);
            }
            _ => graph.unplaced_issues.push(node),
        }
    }

    graph
}

/// The map's `[[edge]]`s as data flow, (upstream, downstream) indices into
/// `ModuleMap::modules`: an edge `from = A, to = B` means A depends on B, so
/// data flows from B into A.
pub fn data_flow(map: &ModuleMap) -> Vec<(usize, usize)> {
    // Edge endpoints are validated against module ids when the map loads.
    let idx = |id: &str| map.modules.iter().position(|m| m.id == id);
    map.edges
        .iter()
        .filter_map(|e| Some((idx(&e.to)?, idx(&e.from)?)))
        .collect()
}

/// The issues to classify this tick: those a live project agent is on.
/// Same linking as `build`, so these are exactly the issues it shows.
pub fn issues_to_classify(world: &World) -> Vec<&Issue> {
    let by_issue = link_agents(world).by_issue;
    world
        .issues
        .iter()
        .filter(|i| by_issue.contains_key(&i.number))
        .collect()
}

fn agent_node(world: &World, facts: &AgentFacts) -> AgentNode {
    let repo = facts.repo.as_ref();
    AgentNode {
        agent: facts.agent.clone(),
        branch: repo.and_then(|r| r.branch.clone()),
        worktree: repo
            .filter(|r| r.toplevel != world.map.root)
            .and_then(|r| r.toplevel.file_name())
            .map(|n| n.to_string_lossy().into_owned()),
        issue: repo
            .filter(|r| !r.on_default_branch)
            .and_then(|r| r.branch.as_deref())
            .and_then(|b| issue_for_branch(b, &world.issues, &world.prs))
            .or_else(|| issue_for_agent_title(&facts.agent, &world.issues)),
    }
}

/// issue → module: the map's module with the highest probability, first in
/// map order on a tie. Probabilities for modules not in the map are ignored.
fn best_module(map: &ModuleMap, classification: &Classification) -> Option<Link<usize>> {
    let mut best: Option<Link<usize>> = None;
    for (i, module) in map.modules.iter().enumerate() {
        let Some(&p) = classification.get(&module.id) else {
            continue;
        };
        if best.as_ref().is_none_or(|b| p > b.confidence) {
            best = Some(Link {
                target: i,
                source: LinkSource::Classifier,
                confidence: p,
            });
        }
    }
    best
}

fn in_project(world: &World, facts: &AgentFacts) -> bool {
    match (&world.project_git_dir, &facts.repo) {
        (Some(project), Some(repo)) => &repo.git_dir == project,
        _ => facts.agent.cwd.starts_with(&world.map.root),
    }
}

/// agent → issue from a branch: number, then issue title, then the branch's PR.
fn issue_for_branch(branch: &str, issues: &[Issue], prs: &[PullRequest]) -> Option<Link<u64>> {
    let known = |n: u64| issues.iter().any(|i| i.number == n);

    if let Some(n) = branch_number(branch).filter(|&n| known(n)) {
        return Some(Link::certain(n, LinkSource::BranchNumber));
    }

    let slug = slugify(branch.rsplit('/').next().unwrap_or(branch));
    // Short slugs like "fix" or "wip" would match too much.
    if slug.len() >= 12
        && let Some(issue) = issues.iter().find(|i| slugify(&i.title).starts_with(&slug))
    {
        return Some(Link::certain(issue.number, LinkSource::TitleMatch));
    }

    prs.iter()
        .filter(|pr| pr.head_branch == branch)
        .flat_map(|pr| &pr.closes)
        .find(|&&n| known(n))
        .map(|&n| Link::certain(n, LinkSource::PullRequest))
}

/// A pane can identify its issue explicitly even when several agents share
/// the default branch. Multiple distinct open issue numbers are ambiguous.
fn issue_for_agent_title(agent: &Agent, issues: &[Issue]) -> Option<Link<u64>> {
    let mut matches = agent
        .title
        .iter()
        .chain(agent.terminal_title.iter())
        .flat_map(|title| {
            title.match_indices('#').filter_map(|(at, _)| {
                let digits: String = title[at + 1..]
                    .chars()
                    .take_while(char::is_ascii_digit)
                    .collect();
                digits.parse::<u64>().ok()
            })
        })
        .filter(|n| issues.iter().any(|issue| issue.number == *n))
        .collect::<Vec<_>>();
    matches.sort_unstable();
    matches.dedup();
    (matches.len() == 1).then(|| Link::certain(matches[0], LinkSource::AgentTitle))
}

/// Issue number from a branch name: a trailing `-104`, or a leading `104-`
/// after any `user/` prefix.
fn branch_number(branch: &str) -> Option<u64> {
    let last = branch.rsplit('/').next()?;
    let trailing = last
        .rsplit(['-', '_'])
        .next()
        .filter(|s| s.len() < last.len());
    let leading = last
        .split(['-', '_'])
        .next()
        .filter(|s| s.len() < last.len());
    trailing
        .and_then(|s| s.parse().ok())
        .or_else(|| leading.and_then(|s| s.parse().ok()))
}

fn slugify(text: &str) -> String {
    let mut slug = String::with_capacity(text.len());
    for c in text.chars().flat_map(char::to_lowercase) {
        if c.is_ascii_alphanumeric() {
            slug.push(c);
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    slug.trim_matches('-').to_string()
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::world::RepoFacts;

    // Modelled on urbandocs.
    const MAP: &str = r#"
        [project]
        name = "urbandocs"

        [[module]]
        id = "server"
        paths = ["src/urbandocs/server.py", "tests/unit/test_server.py"]

        [[module]]
        id = "search"
        paths = ["src/urbandocs/search.py", "tests/unit/test_search.py"]
    "#;

    const GIT_DIR: &str = "/home/u/urbandocs/.git";

    fn issue(number: u64, title: &str) -> Issue {
        Issue {
            number,
            title: title.into(),
            body: String::new(),
            updated_at: String::new(),
            parent: None,
        }
    }

    fn agent(pane: &str, cwd: &str) -> Agent {
        Agent {
            agent: "claude".into(),
            agent_status: "working".into(),
            pane_id: pane.into(),
            cwd: cwd.into(),
            title: None,
            terminal_title: None,
        }
    }

    /// An agent in a urbandocs worktree on `branch`.
    fn on_branch(pane: &str, toplevel: &str, branch: &str) -> AgentFacts {
        AgentFacts {
            agent: agent(pane, toplevel),
            repo: Some(RepoFacts {
                git_dir: GIT_DIR.into(),
                toplevel: toplevel.into(),
                branch: Some(branch.into()),
                on_default_branch: branch == "main",
            }),
        }
    }

    fn world(agents: Vec<AgentFacts>, issues: Vec<Issue>, prs: Vec<PullRequest>) -> World {
        World {
            map: ModuleMap::parse(MAP, Path::new("/home/u/urbandocs")).unwrap(),
            project_git_dir: Some(GIT_DIR.into()),
            agents,
            issues,
            prs,
            classifications: BTreeMap::new(),
        }
    }

    /// A classification from (module id, probability) pairs.
    fn classified(probabilities: &[(&str, f32)]) -> Classification {
        probabilities
            .iter()
            .map(|&(id, p)| (id.to_string(), p))
            .collect()
    }

    /// One agent on issue 102, classified as given.
    fn classified_world(probabilities: &[(&str, f32)]) -> World {
        let mut w = world(
            vec![on_branch("p1", "/home/u/urbandocs", "search-floor-102")],
            vec![issue(102, "Floor search results at score >= 2")],
            vec![],
        );
        w.classifications.insert(102, classified(probabilities));
        w
    }

    fn module_issues(g: &Graph) -> Vec<Vec<u64>> {
        g.modules.iter().map(|m| numbers(&m.issues)).collect()
    }

    fn panes(agents: &[AgentNode]) -> Vec<&str> {
        agents.iter().map(|a| a.agent.pane_id.as_str()).collect()
    }

    fn numbers(issues: &[IssueNode]) -> Vec<u64> {
        issues.iter().map(|i| i.number).collect()
    }

    #[test]
    fn links_agent_by_trailing_branch_number() {
        let g = build(&world(
            vec![on_branch(
                "p1",
                "/home/u/urbandocs",
                "fix-matched-children-score-floor-104",
            )],
            vec![issue(104, "Clear score floor in matched-children search")],
            vec![],
        ));
        let node = &g.unplaced_issues[0];
        assert_eq!(node.number, 104);
        assert_eq!(panes(&node.agents), ["p1"]);
        assert_eq!(
            node.agents[0].issue.as_ref().unwrap().source,
            LinkSource::BranchNumber
        );
    }

    #[test]
    fn shows_only_issues_a_live_agent_is_on() {
        let g = build(&world(
            vec![on_branch(
                "p1",
                "/home/u/urbandocs",
                "search-score-floor-102",
            )],
            vec![
                issue(102, "Floor search results at score >= 2"),
                issue(63, "Render the fidelity disclosure"),
            ],
            vec![],
        ));
        assert_eq!(numbers(&g.unplaced_issues), [102]);
        assert!(
            g.modules.iter().all(|m| m.issues.is_empty()),
            "an unclassified issue is not placed in a module"
        );
    }

    #[test]
    fn links_title_named_branch_in_worktree_outside_repo_folder() {
        let g = build(&world(
            vec![on_branch(
                "p1",
                "/home/u/orca/workspaces/urbandocs/prototype-delegating-search-get-to-a-codex-subag",
                "javier-abia/prototype-delegating-search-get-to-a-codex-subag",
            )],
            vec![issue(
                107,
                "Prototype delegating search→get to a Codex subagent, verbatim-output-first",
            )],
            vec![],
        ));
        assert_eq!(g.elsewhere, 0);
        let node = &g.unplaced_issues[0];
        assert_eq!(node.number, 107);
        let a = &node.agents[0];
        assert_eq!(a.issue.as_ref().unwrap().source, LinkSource::TitleMatch);
        assert_eq!(
            a.worktree.as_deref(),
            Some("prototype-delegating-search-get-to-a-codex-subag")
        );
    }

    #[test]
    fn falls_back_to_the_branch_pull_request() {
        let g = build(&world(
            vec![on_branch("p1", "/home/u/urbandocs", "feat/evals-langfuse")],
            vec![issue(94, "Add a grep-over-PDFs baseline agent")],
            vec![PullRequest {
                head_branch: "feat/evals-langfuse".into(),
                closes: vec![94],
            }],
        ));
        let a = &g.unplaced_issues[0].agents[0];
        assert_eq!(a.issue, Some(Link::certain(94, LinkSource::PullRequest)));
    }

    #[test]
    fn agents_whose_branch_gives_no_issue_go_under_no_issue() {
        let g = build(&world(
            vec![
                // A branch number must not link an agent sitting on main.
                on_branch("p1", "/home/u/urbandocs", "main"),
                on_branch("p2", "/home/u/urbandocs", "wip"),
            ],
            vec![issue(1, "Anything")],
            vec![],
        ));
        assert_eq!(panes(&g.no_issue_agents), ["p1", "p2"]);
        assert!(g.unplaced_issues.is_empty());
    }

    #[test]
    fn links_explicit_issue_numbers_from_live_titles_on_main() {
        let mut from_pane = on_branch("p1", "/home/u/urbandocs", "main");
        from_pane.agent.title = Some("Your organiz".into());
        from_pane.agent.terminal_title = Some("Implement #4 | urbandocs".into());
        let mut from_task = on_branch("p2", "/home/u/urbandocs", "main");
        from_task.agent.title = Some("#5".into());
        let g = build(&world(
            vec![from_pane, from_task],
            vec![issue(4, "Source inventory"), issue(5, "Spatial facts")],
            vec![],
        ));
        assert!(g.no_issue_agents.is_empty());
        assert_eq!(numbers(&g.unplaced_issues), [4, 5]);
        assert_eq!(panes(&g.unplaced_issues[0].agents), ["p1"]);
        assert_eq!(panes(&g.unplaced_issues[1].agents), ["p2"]);
        assert_eq!(
            g.unplaced_issues[0].agents[0]
                .issue
                .as_ref()
                .unwrap()
                .source,
            LinkSource::AgentTitle
        );
    }

    #[test]
    fn ignores_closed_or_ambiguous_issue_numbers_in_agent_titles() {
        let mut closed = on_branch("p1", "/home/u/urbandocs", "main");
        closed.agent.terminal_title = Some("Implement #9 | urbandocs".into());
        let mut ambiguous = on_branch("p2", "/home/u/urbandocs", "main");
        ambiguous.agent.title = Some("Review #4 and #5".into());
        let g = build(&world(
            vec![closed, ambiguous],
            vec![issue(4, "Source inventory"), issue(5, "Spatial facts")],
            vec![],
        ));
        assert_eq!(panes(&g.no_issue_agents), ["p1", "p2"]);
    }

    #[test]
    fn counts_agents_in_other_repos_as_elsewhere() {
        let mut other = on_branch("p1", "/home/u/fintrack", "feat-3");
        other.repo.as_mut().unwrap().git_dir = PathBuf::from("/home/u/fintrack/.git");
        let no_repo = AgentFacts {
            agent: agent("p2", "/home/u"),
            repo: None,
        };
        let g = build(&world(vec![other, no_repo], vec![], vec![]));
        assert_eq!(g.elsewhere, 2);
    }

    #[test]
    fn places_issue_in_its_best_module_at_exactly_min_confidence() {
        let g = build(&classified_world(&[("server", 0.2), ("search", 0.5)]));
        assert_eq!(module_issues(&g), [vec![], vec![102]]);
        assert!(g.unplaced_issues.is_empty());
        let link = g.modules[1].issues[0].module.as_ref().unwrap();
        assert_eq!(link.target, 1);
        assert_eq!(link.source, LinkSource::Classifier);
        assert_eq!(link.confidence, 0.5);
    }

    #[test]
    fn places_issue_in_its_best_module_above_min_confidence() {
        let g = build(&classified_world(&[("server", 0.9), ("search", 0.6)]));
        assert_eq!(module_issues(&g), [vec![102], vec![]]);
        let link = g.modules[0].issues[0].module.as_ref().unwrap();
        assert_eq!((link.target, link.confidence), (0, 0.9));
    }

    #[test]
    fn leaves_issue_below_min_confidence_unplaced_with_its_best_guess() {
        let g = build(&classified_world(&[("server", 0.1), ("search", 0.49)]));
        assert!(g.modules.iter().all(|m| m.issues.is_empty()));
        assert_eq!(numbers(&g.unplaced_issues), [102]);
        let guess = g.unplaced_issues[0].module.as_ref().unwrap();
        assert_eq!(
            (guess.target, guess.source, guess.confidence),
            (1, LinkSource::Classifier, 0.49)
        );
    }

    #[test]
    fn leaves_unclassified_issue_unplaced_with_no_guess() {
        let g = build(&world(
            vec![on_branch("p1", "/home/u/urbandocs", "search-floor-102")],
            vec![issue(102, "Floor search results at score >= 2")],
            vec![],
        ));
        assert_eq!(numbers(&g.unplaced_issues), [102]);
        assert_eq!(g.unplaced_issues[0].module, None);
    }

    #[test]
    fn honours_the_maps_min_confidence() {
        let mut w = classified_world(&[("search", 0.6)]);
        w.map.min_confidence = 0.7;
        let g = build(&w);
        assert_eq!(numbers(&g.unplaced_issues), [102]);
    }

    #[test]
    fn ignores_probabilities_for_modules_not_in_the_map() {
        let g = build(&classified_world(&[("gone", 0.99), ("search", 0.3)]));
        let guess = g.unplaced_issues[0].module.as_ref().unwrap();
        assert_eq!((guess.target, guess.confidence), (1, 0.3));
    }

    #[test]
    fn issues_to_classify_are_only_those_live_project_agents_are_on() {
        let mut elsewhere = on_branch("p2", "/home/u/fintrack", "feat-63");
        elsewhere.repo.as_mut().unwrap().git_dir = PathBuf::from("/home/u/fintrack/.git");
        let w = world(
            vec![
                on_branch("p1", "/home/u/urbandocs", "search-floor-102"),
                elsewhere,
            ],
            vec![
                issue(102, "Floor search results at score >= 2"),
                issue(63, "Render the fidelity disclosure"),
                issue(7, "No agent on this one"),
            ],
            vec![],
        );
        let wanted: Vec<u64> = issues_to_classify(&w).iter().map(|i| i.number).collect();
        assert_eq!(wanted, [102]);
    }

    #[test]
    fn keeps_sub_issue_parent() {
        let mut child = issue(91, "Understand context replay");
        child.parent = Some(IssueRef {
            number: 86,
            title: "Cut token use".into(),
        });
        let g = build(&world(
            vec![on_branch("p1", "/home/u/urbandocs", "context-replay-91")],
            vec![child],
            vec![],
        ));
        assert_eq!(g.unplaced_issues[0].parent.as_ref().unwrap().number, 86);
    }

    #[test]
    fn data_flows_from_the_dependency_into_the_dependent() {
        let map = ModuleMap::parse(
            &format!("{MAP}\n[[edge]]\nfrom = \"server\"\nto = \"search\"\n"),
            Path::new("/home/u/urbandocs"),
        )
        .unwrap();
        // server depends on search: search (1) feeds the server (0).
        assert_eq!(data_flow(&map), [(1, 0)]);
    }

    #[test]
    fn parses_branch_numbers() {
        assert_eq!(branch_number("search-cap-benchmark-90"), Some(90));
        assert_eq!(branch_number("user/42-add-thing"), Some(42));
        assert_eq!(branch_number("feat/evals-langfuse"), None);
        assert_eq!(branch_number("123"), None);
    }
}
