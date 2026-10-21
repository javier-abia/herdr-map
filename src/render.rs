//! Plain-text renderer: modules, the issues under them, the agents on those
//! issues. A thin view over `Graph`; all decisions are made in `graph`.

use std::fmt::Write;

use crate::graph::{AgentNode, Graph, IssueNode, LinkSource};
use crate::modulemap::ModuleMap;

pub struct Style {
    pub color: bool,
}

impl Style {
    fn paint(&self, code: &str, text: &str) -> String {
        if self.color {
            format!("\x1b[{code}m{text}\x1b[0m")
        } else {
            text.to_string()
        }
    }
    fn dim(&self, text: &str) -> String {
        self.paint("2", text)
    }
    fn bold(&self, text: &str) -> String {
        self.paint("1", text)
    }
}

fn status_code(status: &str) -> &'static str {
    match status {
        "working" => "32",           // green
        "blocked" | "error" => "31", // red
        "idle" | "done" => "33",     // yellow
        _ => "2",                    // dim
    }
}

fn status_dot(status: &str) -> &'static str {
    match status {
        "working" => "●",
        "blocked" | "error" => "!",
        _ => "○",
    }
}

/// The "busiest" status among some agents, for a module's dot.
fn busiest<'a>(agents: impl Iterator<Item = &'a AgentNode>) -> Option<&'a str> {
    let statuses: Vec<&str> = agents.map(|a| a.agent.agent_status.as_str()).collect();
    ["blocked", "error", "working"]
        .into_iter()
        .find(|s| statuses.contains(s))
        .or_else(|| statuses.first().copied())
}

fn dot(status: Option<&str>, style: &Style) -> String {
    match status {
        Some(s) => style.paint(status_code(s), status_dot(s)),
        None => style.dim("·"),
    }
}

fn agent_chip(a: &AgentNode, style: &Style) -> String {
    let s = &a.agent.agent_status;
    let mut chip = format!(
        "{} {} {}",
        style.paint(status_code(s), status_dot(s)),
        a.agent.agent,
        style.dim(&a.agent.pane_id),
    );
    // The branch is shown when available; the worktree folder stands in for
    // it on a detached HEAD.
    if let Some(label) = a.branch.as_ref().or(a.worktree.as_ref()) {
        let _ = write!(chip, " {}", style.paint("36", &format!("⎇ {label}")));
    }
    match a.issue.as_ref().map(|l| l.source) {
        Some(LinkSource::TitleMatch) => chip.push_str(&style.dim(" (title match)")),
        Some(LinkSource::PullRequest) => chip.push_str(&style.dim(" (via PR)")),
        Some(LinkSource::AgentTitle) => chip.push_str(&style.dim(" (agent title)")),
        _ => {}
    }
    if let Some(title) = a.agent.title.as_deref().filter(|t| !t.is_empty()) {
        let _ = write!(chip, " \"{title}\"");
    }
    if s != "working" {
        chip.push_str(&style.dim(&format!(" ({s})")));
    }
    chip
}

const INDENT: &str = "     ";

/// What follows an issue's title: its probability when placed under a
/// module, its best guess when not placed.
#[derive(Clone, Copy)]
enum ModuleAnnotation {
    Probability,
    BestGuess,
}

fn render_issues(
    out: &mut String,
    map: &ModuleMap,
    issues: &[IssueNode],
    annotation: ModuleAnnotation,
    style: &Style,
) {
    // Top-level issues first, then sub-issues grouped under their parent.
    let mut order: Vec<&IssueNode> = issues.iter().collect();
    order.sort_by_key(|i| (i.parent.as_ref().map(|p| p.number), i.number));

    let mut current_parent = None;
    for issue in order {
        let mut indent = INDENT.to_string();
        if let Some(parent) = &issue.parent {
            if current_parent != Some(parent.number) {
                current_parent = Some(parent.number);
                let _ = writeln!(
                    out,
                    "{INDENT}{}",
                    style.dim(&format!("#{} {}", parent.number, parent.title))
                );
            }
            indent.push_str("  ");
        }

        let _ = write!(
            out,
            "{indent}{} {}",
            style.bold(&format!("#{}", issue.number)),
            issue.title
        );
        if let Some(link) = &issue.module {
            let note = match annotation {
                ModuleAnnotation::Probability => format!("  {:.2}", link.confidence),
                ModuleAnnotation::BestGuess => format!(
                    " (best guess: {} {:.2})",
                    map.modules[link.target].label, link.confidence
                ),
            };
            out.push_str(&style.dim(&note));
        }
        out.push('\n');

        for a in &issue.agents {
            let _ = writeln!(out, "{indent}  {}", agent_chip(a, style));
        }
    }
}

/// Draws one frame. `errors` (the classifier or its cache failed this tick)
/// go on top, one `✗` line each; `warnings` (a source serving stale data, like
/// GitHub) go at the bottom, one `!` line each. The graph in between is drawn
/// from whatever data there is.
pub fn render(
    map: &ModuleMap,
    graph: &Graph,
    errors: &[String],
    warnings: &[String],
    style: &Style,
) -> String {
    let mut out = String::new();
    for e in errors {
        let _ = writeln!(out, "{}", style.paint("31", &format!("✗ {e}")));
    }

    let agents: Vec<&AgentNode> = graph
        .modules
        .iter()
        .flat_map(|m| m.issues.iter().flat_map(|i| &i.agents))
        .chain(graph.unplaced_issues.iter().flat_map(|i| &i.agents))
        .chain(&graph.no_issue_agents)
        .collect();
    let working = agents
        .iter()
        .filter(|a| a.agent.agent_status == "working")
        .count();
    let _ = write!(
        out,
        "{} · {} agent{} ({working} working)",
        style.bold(&map.name),
        agents.len(),
        if agents.len() == 1 { "" } else { "s" },
    );
    if graph.elsewhere > 0 {
        out.push_str(&style.dim(&format!(" · {} elsewhere", graph.elsewhere)));
    }
    out.push_str("\n\n");

    for (module, node) in map.modules.iter().zip(&graph.modules) {
        let status = busiest(node.issues.iter().flat_map(|i| &i.agents));
        let empty = node.issues.is_empty();
        let label = if empty {
            style.dim(&module.label)
        } else {
            style.bold(&module.label)
        };
        let _ = write!(out, " {} {label}", dot(status, style));
        if empty {
            out.push_str(&style.dim("  —"));
        }
        out.push('\n');
        render_issues(
            &mut out,
            map,
            &node.issues,
            ModuleAnnotation::Probability,
            style,
        );
    }

    if !graph.unplaced_issues.is_empty() {
        let _ = writeln!(
            out,
            "\n {} {}",
            style.dim("?"),
            style.bold("Not placed in a module")
        );
        render_issues(
            &mut out,
            map,
            &graph.unplaced_issues,
            ModuleAnnotation::BestGuess,
            style,
        );
    }
    if !graph.no_issue_agents.is_empty() {
        let _ = writeln!(out, "\n {} {}", style.dim("?"), style.bold("No issue"));
        for a in &graph.no_issue_agents {
            let _ = writeln!(out, "{INDENT}{}", agent_chip(a, style));
        }
    }

    if !map.edges.is_empty() {
        let _ = writeln!(out, "\n {}", style.dim("depends on"));
        // Edge endpoints are validated against module ids when the map loads.
        let label = |id: &str| -> &str {
            map.modules
                .iter()
                .find(|m| m.id == id)
                .map_or("?", |m| m.label.as_str())
        };
        for e in &map.edges {
            let _ = writeln!(out, "   {} ─▶ {}", label(&e.from), label(&e.to));
        }
    }

    for w in warnings {
        let _ = writeln!(out, "\n {}", style.paint("31", &format!("! {w}")));
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{Link, ModuleNode};
    use crate::herdr::Agent;
    use std::path::PathBuf;

    fn node(branch: Option<&str>, worktree: Option<&str>) -> AgentNode {
        AgentNode {
            agent: Agent {
                agent: "claude".into(),
                agent_status: "working".into(),
                pane_id: "w1:p1".into(),
                cwd: PathBuf::from("/repo"),
                title: None,
                terminal_title: None,
            },
            branch: branch.map(Into::into),
            worktree: worktree.map(Into::into),
            issue: None,
        }
    }

    #[test]
    fn chip_shows_branch_over_worktree_folder() {
        let chip = agent_chip(
            &node(Some("tool-schema"), Some("srv")),
            &Style { color: false },
        );
        assert!(chip.contains("⎇ tool-schema"), "{chip}");
        assert!(!chip.contains("srv"), "{chip}");
    }

    #[test]
    fn lists_agents_without_an_issue_under_no_issue() {
        let map =
            ModuleMap::parse("[[module]]\nid = \"a\"\n", std::path::Path::new("/repo")).unwrap();
        let graph = Graph {
            modules: vec![Default::default()],
            no_issue_agents: vec![node(Some("main"), None)],
            ..Default::default()
        };
        let out = render(&map, &graph, &[], &[], &Style { color: false });
        assert!(out.contains("? No issue\n"), "{out}");
        assert!(out.contains("⎇ main"), "{out}");
    }

    fn issue_node(number: u64, title: &str, module: Option<(usize, f32)>) -> IssueNode {
        IssueNode {
            number,
            title: title.into(),
            parent: None,
            module: module.map(|(target, confidence)| Link {
                target,
                source: LinkSource::Classifier,
                confidence,
            }),
            agents: vec![node(Some("b"), None)],
        }
    }

    const TWO_MODULES: &str = "[[module]]\nid = \"server\"\nlabel = \"MCP server\"\n[[module]]\nid = \"search\"\nlabel = \"Search\"\n";

    #[test]
    fn shows_the_probability_after_a_placed_issue() {
        let map = ModuleMap::parse(TWO_MODULES, std::path::Path::new("/repo")).unwrap();
        let graph = Graph {
            modules: vec![
                Default::default(),
                ModuleNode {
                    issues: vec![issue_node(102, "Floor scores", Some((1, 0.82)))],
                },
            ],
            ..Default::default()
        };
        let out = render(&map, &graph, &[], &[], &Style { color: false });
        assert!(out.contains("#102 Floor scores  0.82\n"), "{out}");
    }

    #[test]
    fn shows_the_best_guess_after_an_unplaced_issue_and_nothing_without_one() {
        let map = ModuleMap::parse(TWO_MODULES, std::path::Path::new("/repo")).unwrap();
        let graph = Graph {
            modules: vec![Default::default(), Default::default()],
            unplaced_issues: vec![
                issue_node(102, "Floor scores", Some((1, 0.42))),
                issue_node(103, "Unclassified", None),
            ],
            ..Default::default()
        };
        let out = render(&map, &graph, &[], &[], &Style { color: false });
        assert!(
            out.contains("#102 Floor scores (best guess: Search 0.42)\n"),
            "{out}"
        );
        assert!(out.contains("#103 Unclassified\n"), "{out}");
    }

    #[test]
    fn errors_go_on_top_and_the_graph_still_renders() {
        let map = ModuleMap::parse(TWO_MODULES, std::path::Path::new("/repo")).unwrap();
        let graph = Graph {
            modules: vec![Default::default(), Default::default()],
            unplaced_issues: vec![issue_node(102, "Floor scores", None)],
            ..Default::default()
        };
        let errors = ["HERDR_MAP_OPENROUTER_API_KEY not set".to_string()];
        let out = render(&map, &graph, &errors, &[], &Style { color: false });
        assert!(
            out.starts_with("✗ HERDR_MAP_OPENROUTER_API_KEY not set\n"),
            "{out}"
        );
        assert!(out.contains("#102 Floor scores\n"), "{out}");
    }

    #[test]
    fn warnings_go_at_the_bottom_apart_from_the_errors_on_top() {
        let map = ModuleMap::parse(TWO_MODULES, std::path::Path::new("/repo")).unwrap();
        let graph = Graph {
            modules: vec![Default::default(), Default::default()],
            ..Default::default()
        };
        let errors = ["OpenRouter unreachable: 401 User not found".to_string()];
        let warnings = ["GitHub unavailable, showing cached issues: offline".to_string()];
        let out = render(&map, &graph, &errors, &warnings, &Style { color: false });
        assert!(
            out.starts_with("✗ OpenRouter unreachable: 401 User not found\n"),
            "{out}"
        );
        assert!(
            out.ends_with("\n ! GitHub unavailable, showing cached issues: offline\n"),
            "{out}"
        );
        assert!(!out.contains("✗ GitHub"), "{out}");
    }

    #[test]
    fn chip_falls_back_to_worktree_on_detached_head() {
        let chip = agent_chip(&node(None, Some("srv")), &Style { color: false });
        assert!(chip.contains("⎇ srv"), "{chip}");
    }
}
