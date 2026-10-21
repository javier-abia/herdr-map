//! Draws the graph as a data-flow diagram: one box per module, in the rows
//! `flow::layers` (stacked) or `flow::chain_rows` (chains) gives, joined by
//! arrows that point the way data flows.
//! Issues with no module and agents with no issue go in a tray of boxes
//! underneath. Pure: the whole diagram is drawn into a buffer as tall as it
//! needs, and the caller shows the part that fits.

use std::collections::HashMap;

use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Padding, Paragraph, Widget};

use super::flow::{MapInfo, chain_rows, layers};
use crate::graph::{AgentNode, Graph, IssueNode};

pub const C_ACCENT: Color = Color::Rgb(125, 211, 252);
pub const C_NUM: Color = Color::Rgb(103, 232, 249);
const C_ACTIVE: Color = Color::Rgb(134, 239, 172);
const C_IDLE: Color = Color::Rgb(75, 85, 99);
const C_EDGE: Color = Color::Rgb(100, 116, 139);
const C_WARN: Color = Color::Rgb(250, 204, 21);
const C_BLOCKED: Color = Color::Rgb(248, 113, 113);
const C_SELECTED_BG: Color = Color::Rgb(30, 58, 74);

pub fn dim() -> Style {
    Style::new().fg(Color::DarkGray)
}

pub fn status_color(status: &str) -> Color {
    match status {
        "working" => Color::Green,
        "blocked" | "error" => Color::Red,
        "idle" | "done" => Color::Yellow,
        _ => Color::DarkGray,
    }
}

pub fn dot(status: &str) -> Span<'static> {
    let d = match status {
        "working" => "●",
        "blocked" | "error" => "!",
        _ => "○",
    };
    Span::styled(d, Style::new().fg(status_color(status)).bold())
}

/// The "busiest" status among some agents, for a box's title dot.
fn busiest<'a>(statuses: &[&'a str]) -> Option<&'a str> {
    ["blocked", "error", "working"]
        .into_iter()
        .find(|s| statuses.contains(s))
        .or_else(|| statuses.first().copied())
}

/// The branch is what links an agent to an issue; the worktree folder name
/// only stands in for it on a detached HEAD.
pub fn branch(a: &AgentNode) -> &str {
    a.branch.as_deref().or(a.worktree.as_deref()).unwrap_or("")
}

/// One agent in selection order, with where it sits in the diagram.
pub struct Entry<'a> {
    pub agent: &'a AgentNode,
    pub issue: Option<&'a IssueNode>,
    /// The module the issue is placed under (None: unplaced or no issue).
    pub module: Option<usize>,
}

/// Top-level issues first, then sub-issues grouped under their parent.
fn sorted(issues: &[IssueNode]) -> Vec<&IssueNode> {
    let mut v: Vec<&IssueNode> = issues.iter().collect();
    v.sort_by_key(|i| (i.parent.as_ref().map(|p| p.number), i.number));
    v
}

/// How modules are placed in rows.
#[derive(Clone, Copy, Debug, Default, PartialEq, clap::ValueEnum)]
pub enum Placement {
    /// One data-flow layer per row; arrows always go down.
    #[default]
    Stacked,
    /// Chains of modules, each the only input of the next, go sideways and
    /// wrap like text; a layer's modules and chains share lines.
    Chains,
}

/// How arrows that skip a row are drawn.
#[derive(Clone, Copy, Debug, Default, PartialEq, clap::ValueEnum)]
pub enum Arrows {
    /// One lane down the left margin per source module, branching off to
    /// each target.
    #[default]
    Trunks,
    /// Not drawn: the target box names its sources on its first line.
    Stubs,
}

impl Placement {
    pub fn next(self) -> Self {
        match self {
            Self::Stacked => Self::Chains,
            Self::Chains => Self::Stacked,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Stacked => "stacked",
            Self::Chains => "chains",
        }
    }
}

impl Arrows {
    pub fn next(self) -> Self {
        match self {
            Self::Trunks => Self::Stubs,
            Self::Stubs => Self::Trunks,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Trunks => "trunks",
            Self::Stubs => "stubs",
        }
    }
}

/// How the diagram is laid out. The default is stacked with trunks.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Modes {
    pub placement: Placement,
    pub arrows: Arrows,
}

/// Rows of modules as `placement` reads them, top to bottom.
fn module_rows(map: &MapInfo, placement: Placement) -> Vec<Vec<usize>> {
    match placement {
        Placement::Stacked => layers(map),
        Placement::Chains => chain_rows(map),
    }
}

/// Every agent in the order the diagram reads: modules row by row, left to
/// right, then unplaced issues, then agents with no issue.
pub fn entries<'a>(map: &MapInfo, g: &'a Graph, placement: Placement) -> Vec<Entry<'a>> {
    let mut out = Vec::new();
    for m in module_rows(map, placement).concat() {
        for issue in sorted(&g.modules[m].issues) {
            out.extend(issue.agents.iter().map(|agent| Entry {
                agent,
                issue: Some(issue),
                module: Some(m),
            }));
        }
    }
    for issue in sorted(&g.unplaced_issues) {
        out.extend(issue.agents.iter().map(|agent| Entry {
            agent,
            issue: Some(issue),
            module: None,
        }));
    }
    out.extend(g.no_issue_agents.iter().map(|agent| Entry {
        agent,
        issue: None,
        module: None,
    }));
    out
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(max.saturating_sub(1)).collect();
        t.push('…');
        t
    }
}

/// What goes in a box.
#[derive(Clone, Copy)]
enum Content<'a> {
    Module(usize),
    Unplaced(&'a [IssueNode]),
    NoIssue(&'a [AgentNode]),
}

/// A box in the diagram, and where it goes once placed.
#[derive(Clone)]
struct GBox<'a> {
    content: Content<'a>,
    title: String,
    rect: Rect,
}

impl<'a> GBox<'a> {
    fn new(content: Content<'a>, title: String) -> Self {
        Self {
            content,
            title,
            rect: Rect::default(),
        }
    }

    fn module(&self) -> Option<usize> {
        match self.content {
            Content::Module(m) => Some(m),
            _ => None,
        }
    }
}

/// What is selected, as the diagram needs it.
struct Selection<'a> {
    pane: &'a str,
    module: Option<usize>,
    has_issue: bool,
}

fn chip(a: &AgentNode, selected: bool, with_branch: bool) -> Line<'static> {
    let s = &a.agent.agent_status;
    let mut spans = vec![
        Span::from("  "),
        dot(s),
        Span::from(format!(" {} ", a.agent.agent)),
        Span::styled(a.agent.pane_id.clone(), dim()),
    ];
    if with_branch {
        spans.push(Span::styled(
            format!(" ⎇ {}", branch(a)),
            Style::new().fg(C_NUM),
        ));
    }
    if s != "working" {
        spans.push(Span::styled(
            format!(" {s}"),
            Style::new().fg(status_color(s)),
        ));
    }
    let line = Line::from(spans);
    if selected {
        line.style(Style::new().bg(C_SELECTED_BG).bold())
    } else {
        line
    }
}

fn issue_lines(
    map: &MapInfo,
    issues: &[IssueNode],
    inner: usize,
    best_guess: bool,
    sel: Option<&str>,
) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    let mut current_parent = None;
    for i in sorted(issues) {
        // Sub-issues are indented under a header for their parent.
        let indent = match &i.parent {
            Some(p) => {
                if current_parent != Some(p.number) {
                    current_parent = Some(p.number);
                    out.push(Line::styled(
                        truncate(&format!("#{} {}", p.number, p.title), inner),
                        dim(),
                    ));
                }
                "  "
            }
            None => "",
        };
        let num = format!("{indent}#{}", i.number);
        let prob = match (&i.module, best_guess) {
            (Some(m), false) => format!("  {:.2}", m.confidence),
            _ => String::new(),
        };
        let room = inner.saturating_sub(num.len() + 1 + prob.len());
        out.push(Line::from(vec![
            Span::styled(num, Style::new().fg(C_NUM).bold()),
            Span::from(format!(" {}", truncate(&i.title, room))),
            Span::styled(prob, dim()),
        ]));
        if best_guess {
            let guess = match &i.module {
                Some(m) => format!("  best guess {} {:.2}", map.labels[m.target], m.confidence),
                None => "  unclassified".into(),
            };
            out.push(Line::styled(truncate(&guess, inner), dim().italic()));
        }
        for a in &i.agents {
            out.push(chip(a, sel == Some(a.agent.pane_id.as_str()), false));
        }
    }
    out
}

/// A box's lines at a given inner width.
fn box_lines(
    map: &MapInfo,
    g: &Graph,
    content: &Content,
    inner: usize,
    sel: Option<&str>,
) -> Vec<Line<'static>> {
    match content {
        Content::Module(m) if g.modules[*m].issues.is_empty() => {
            vec![Line::styled("no agents", Style::new().fg(C_IDLE)).centered()]
        }
        Content::Module(m) => issue_lines(map, &g.modules[*m].issues, inner, false, sel),
        Content::Unplaced(issues) => issue_lines(map, issues, inner, true, sel),
        Content::NoIssue(agents) => agents
            .iter()
            .map(|a| chip(a, sel == Some(a.agent.pane_id.as_str()), true))
            .collect(),
    }
}

fn agents_in<'a>(g: &'a Graph, content: &Content<'a>) -> Vec<&'a AgentNode> {
    match content {
        Content::Module(m) => g.modules[*m]
            .issues
            .iter()
            .flat_map(|i| &i.agents)
            .collect(),
        Content::Unplaced(issues) => issues.iter().flat_map(|i| &i.agents).collect(),
        Content::NoIssue(agents) => agents.iter().collect(),
    }
}

const UP: u8 = 1;
const DOWN: u8 = 2;
const LEFT: u8 = 4;
const RIGHT: u8 = 8;

/// Arrow cells as connection bits, so where arrows join or cross they get
/// the right box-drawing character.
#[derive(Default)]
struct Wires {
    cells: HashMap<(u16, u16), (u8, bool)>,
    /// Arrowheads: where, lit or not, and which way they point.
    heads: Vec<(u16, u16, bool, &'static str)>,
}

impl Wires {
    fn bit(&mut self, x: u16, y: u16, b: u8, hot: bool) {
        let c = self.cells.entry((x, y)).or_default();
        c.0 |= b;
        c.1 |= hot;
    }

    /// A path of horizontal and vertical segments through `pts`.
    fn path(&mut self, pts: &[(u16, u16)], hot: bool) {
        for w in pts.windows(2) {
            let ((x1, y1), (x2, y2)) = (w[0], w[1]);
            if y1 == y2 {
                let (lo, hi) = (x1.min(x2), x1.max(x2));
                for x in lo..=hi {
                    if x > lo {
                        self.bit(x, y1, LEFT, hot);
                    }
                    if x < hi {
                        self.bit(x, y1, RIGHT, hot);
                    }
                }
            } else {
                let (lo, hi) = (y1.min(y2), y1.max(y2));
                for y in lo..=hi {
                    if y > lo {
                        self.bit(x1, y, UP, hot);
                    }
                    if y < hi {
                        self.bit(x1, y, DOWN, hot);
                    }
                }
            }
        }
    }

    fn draw(&self, buf: &mut Buffer) {
        let color = |hot: bool| if hot { C_ACCENT } else { C_EDGE };
        for (&(x, y), &(b, hot)) in &self.cells {
            // A lone end (UP alone, LEFT alone…) draws as a straight line.
            let sym = match b {
                UP | DOWN => "│",
                b if b == UP | DOWN => "│",
                LEFT | RIGHT => "─",
                b if b == LEFT | RIGHT => "─",
                b if b == DOWN | RIGHT => "╭",
                b if b == DOWN | LEFT => "╮",
                b if b == UP | RIGHT => "╰",
                b if b == UP | LEFT => "╯",
                b if b == UP | DOWN | RIGHT => "├",
                b if b == UP | DOWN | LEFT => "┤",
                b if b == DOWN | LEFT | RIGHT => "┬",
                b if b == UP | LEFT | RIGHT => "┴",
                _ => "┼",
            };
            if buf.area.contains(Position { x, y }) {
                buf[(x, y)].set_symbol(sym).set_fg(color(hot));
            }
        }
        for &(x, y, hot, head) in &self.heads {
            if buf.area.contains(Position { x, y }) {
                buf[(x, y)].set_symbol(head).set_fg(color(hot));
            }
        }
    }
}

/// Wide enough for a sideways arrow `─▶` between two boxes: at least 3.
const GAP_X: u16 = 4;
const GAP_Y: u16 = 2;
const MIN_BOX: u16 = 22;
const MAX_BOX: u16 = 50;

/// Which border an arrow leaves a box by.
#[derive(Clone, Copy)]
enum Side {
    Bottom,
    Right,
}

/// A line of boxes as laid out.
struct Row<'a> {
    boxes: Vec<GBox<'a>>,
    /// Carries on the line above: anchored to its left edge, not centred.
    cont: bool,
}

impl<'a> Row<'a> {
    fn new(boxes: &[GBox<'a>]) -> Self {
        Self {
            boxes: boxes.to_vec(),
            cont: false,
        }
    }
}

/// `boxes` on lines `avail` wide, filled by width like words; every line
/// after the first carries on the one before.
fn pack<'a>(boxes: &[GBox<'a>], avail: u16, width: impl Fn(&GBox) -> u16) -> Vec<Row<'a>> {
    let mut rows: Vec<Row> = Vec::new();
    let mut used = 0;
    for b in boxes {
        let w = width(b);
        match rows.last_mut() {
            Some(line) if used + GAP_X + w <= avail => {
                used += GAP_X + w;
                line.boxes.push(b.clone());
            }
            _ => {
                used = w;
                rows.push(Row {
                    boxes: vec![b.clone()],
                    cont: !rows.is_empty(),
                });
            }
        }
    }
    rows
}

/// The modules long arrows leave from, in order: one trunk each.
fn trunks(long: &[(usize, usize)]) -> Vec<usize> {
    let mut sources: Vec<usize> = long.iter().map(|&(up, _)| up).collect();
    sources.sort();
    sources.dedup();
    sources
}

/// The whole diagram, and where the selected agent's box is in it.
pub struct Diagram {
    pub buf: Buffer,
    pub selected: Option<Rect>,
}

/// Draws the diagram `width` columns wide, laid out as `modes` says, with
/// `sel` highlighted.
pub fn draw(map: &MapInfo, g: &Graph, sel: Option<&Entry>, width: u16, modes: Modes) -> Diagram {
    let sel = sel.map(|e| Selection {
        pane: &e.agent.agent.pane_id,
        module: e.module,
        has_issue: e.issue.is_some(),
    });
    let sel_pane = sel.as_ref().map(|s| s.pane);
    let sel_module = sel.as_ref().and_then(|s| s.module);

    // A box's width before any squeeze.
    let natural = |b: &GBox| {
        let widest = box_lines(map, g, &b.content, usize::MAX, sel_pane)
            .iter()
            .map(Line::width)
            .max()
            .unwrap_or(0) as u16;
        (widest + 4)
            .max(b.title.chars().count() as u16 + 8)
            .clamp(MIN_BOX, MAX_BOX)
    };

    // Rows of boxes: the modules' rows, then a tray for what has no module.
    let module_rows: Vec<Vec<GBox>> = module_rows(map, modes.placement)
        .into_iter()
        .map(|row| {
            row.into_iter()
                .map(|m| GBox::new(Content::Module(m), map.labels[m].to_uppercase()))
                .collect()
        })
        .collect();
    let mut tray = Vec::new();
    if !g.unplaced_issues.is_empty() {
        tray.push(GBox::new(
            Content::Unplaced(&g.unplaced_issues),
            "NOT PLACED".into(),
        ));
    }
    if !g.no_issue_agents.is_empty() {
        tray.push(GBox::new(
            Content::NoIssue(&g.no_issue_agents),
            "NO ISSUE".into(),
        ));
    }

    // A row too wide for the screen carries on below: stacked, by how many
    // boxes fit; chains, filled by width. Trunks for arrows that skip a row
    // run down a margin on the left. The margin takes width, which can wrap
    // more rows: repeat until it settles. It only grows, and is capped, so
    // this ends.
    let max_margin = width / 4;
    let mut margin = 0;
    let (mut rows, tray_row, long) = loop {
        let avail = width.saturating_sub(margin);
        let fit = ((avail + GAP_X) / (MIN_BOX + GAP_X)).max(1) as usize;
        let mut rows: Vec<Row> = Vec::new();
        for row in &module_rows {
            match modes.placement {
                Placement::Stacked => rows.extend(row.chunks(fit).map(Row::new)),
                Placement::Chains => rows.extend(pack(row, avail, natural)),
            }
        }
        let tray_row = (!tray.is_empty()).then_some(rows.len());
        rows.extend(tray.chunks(fit).map(Row::new));

        let row_of: HashMap<usize, usize> = rows
            .iter()
            .enumerate()
            .flat_map(|(ri, row)| {
                row.boxes
                    .iter()
                    .filter_map(move |b| Some((b.module()?, ri)))
            })
            .collect();
        let long: Vec<(usize, usize)> = map
            .flow
            .iter()
            .copied()
            .filter(|(a, b)| {
                matches!((row_of.get(a), row_of.get(b)), (Some(ra), Some(rb)) if *rb > ra + 1)
            })
            .collect();
        let lanes = match modes.arrows {
            Arrows::Trunks => trunks(&long).len(),
            Arrows::Stubs => 0,
        };
        let wanted = (2 * lanes as u16).min(max_margin);
        if wanted <= margin {
            break (rows, tray_row, long);
        }
        margin = wanted;
    };
    // More trunks than lanes share them.
    let lanes = usize::from(margin / 2).max(1);

    // Stubs: a box's first line names the modules whose long arrows into it
    // are not drawn.
    let mut stubs: HashMap<usize, Vec<usize>> = HashMap::new();
    if modes.arrows == Arrows::Stubs {
        for &(up, down) in &long {
            stubs.entry(down).or_default().push(up);
        }
    }
    let lines_for = |b: &GBox, inner: usize| {
        let mut lines = Vec::new();
        if let Some(ups) = b.module().and_then(|m| stubs.get(&m)) {
            let hot = sel_module.is_some()
                && (sel_module == b.module() || ups.iter().any(|&u| sel_module == Some(u)));
            let names: Vec<&str> = ups.iter().map(|&u| map.labels[u].as_str()).collect();
            lines.push(Line::styled(
                truncate(&format!("◂ {}", names.join(", ")), inner),
                Style::new()
                    .fg(if hot { C_ACCENT } else { C_EDGE })
                    .italic(),
            ));
        }
        lines.extend(box_lines(map, g, &b.content, inner, sel_pane));
        lines
    };

    // Size and place the boxes: natural width, squeezed to fit the line,
    // each row's first line centred and the rest under its left edge.
    let avail = width.saturating_sub(margin);
    let mut y: u16 = 0;
    let mut left = margin;
    for (ri, row) in rows.iter_mut().enumerate() {
        if Some(ri) == tray_row {
            // The tray has no arrows into it, so it needs no gap for them.
            y = y.saturating_sub(GAP_Y) + 1;
        }
        let k = row.boxes.len() as u16;
        let gaps = GAP_X * (k - 1);
        let mut widths: Vec<u16> = row.boxes.iter().map(natural).collect();
        if widths.iter().sum::<u16>() + gaps > avail {
            let w = (avail.saturating_sub(gaps) / k).max(5);
            widths.iter_mut().for_each(|x| *x = w);
        }
        let total = widths.iter().sum::<u16>() + gaps;
        left = if row.cont {
            left.min(width.saturating_sub(total)).max(margin)
        } else {
            margin + avail.saturating_sub(total) / 2
        };
        let mut x = left;
        let mut tallest = 0;
        for (b, w) in row.boxes.iter_mut().zip(widths) {
            let h = lines_for(b, (w - 4) as usize).len() as u16 + 2;
            b.rect = Rect::new(x, y, w, h);
            x += w + GAP_X;
            tallest = tallest.max(h);
        }
        y += tallest + GAP_Y;
    }
    let height = y.saturating_sub(GAP_Y);
    let mut buf = Buffer::empty(Rect::new(0, 0, width, height));

    let boxes = || rows.iter().flat_map(|r| &r.boxes);
    let rects: HashMap<usize, Rect> = boxes()
        .filter_map(|b| Some((b.module()?, b.rect)))
        .collect();
    let row_bottom = |r: &Rect| {
        boxes()
            .filter(|b| b.rect.y == r.y)
            .map(|b| b.rect.bottom())
            .max()
            .unwrap_or(r.bottom())
    };

    // Arrows first; boxes are drawn over them.
    let mut wires = Wires::default();
    let mut joints = Vec::new();
    let trunks = trunks(&long);
    for &(up, down) in &map.flow {
        let (Some(ra), Some(rb)) = (rects.get(&up), rects.get(&down)) else {
            continue;
        };
        let hot = sel_module == Some(up) || sel_module == Some(down);
        if rb.y == ra.y {
            // Same line: sideways, out of the source's right side. Only
            // neighbours in a chain share an edge on a line, so it crosses
            // no box.
            if rb.x >= ra.right() + 2 {
                let y = ra.y + 1;
                wires.path(&[(ra.right(), y), (rb.x - 2, y)], hot);
                wires.heads.push((rb.x - 1, y, hot, "▶"));
                joints.push((ra.right() - 1, y, hot, Side::Right));
            }
            continue;
        }
        if rb.y <= ra.bottom() {
            continue; // back up a cycle: not drawn
        }
        let lane = long
            .contains(&(up, down))
            .then(|| trunks.binary_search(&up).unwrap_or_default());
        if lane.is_some() && modes.arrows == Arrows::Stubs {
            continue; // named inside the target box instead
        }
        let xa = ra.x + ra.width / 2;
        // Straight down when the upstream's centre is already over the target.
        let xb = if (rb.x + 2..rb.right().saturating_sub(2)).contains(&xa) {
            xa
        } else {
            rb.x + rb.width / 2
        };
        let g0 = row_bottom(ra);
        let (t0, t1) = (rb.y - 2, rb.y - 1);
        let pts = match lane {
            None => vec![(xa, ra.bottom()), (xa, g0), (xb, g0), (xb, t1)],
            Some(k) => {
                let cx = 1 + 2 * (k % lanes) as u16;
                vec![
                    (xa, ra.bottom()),
                    (xa, g0),
                    (cx, g0),
                    (cx, t0),
                    (xb, t0),
                    (xb, t1),
                ]
            }
        };
        wires.bit(xa, ra.bottom(), UP, hot);
        wires.path(&pts, hot);
        wires.heads.push((xb, t1, hot, "▼"));
        joints.push((xa, ra.bottom() - 1, hot, Side::Bottom));
    }
    wires.draw(&mut buf);

    let mut selected = None;
    for b in boxes() {
        let clip = b.rect.intersection(buf.area);
        let agents = agents_in(g, &b.content);
        let statuses: Vec<&str> = agents
            .iter()
            .map(|a| a.agent.agent_status.as_str())
            .collect();
        let busy = busiest(&statuses);
        let is_selected = sel.as_ref().is_some_and(|s| match b.content {
            Content::Module(m) => s.module == Some(m),
            Content::Unplaced(_) => s.module.is_none() && s.has_issue,
            Content::NoIssue(_) => !s.has_issue,
        });
        if is_selected {
            selected = Some(b.rect);
        }
        let color = match (&b.content, busy) {
            _ if is_selected => C_ACCENT,
            (_, Some("blocked" | "error")) => C_BLOCKED,
            (Content::Module(_), Some(_)) => C_ACTIVE,
            // Idle: a module no agent is on.
            (Content::Module(_), None) => C_IDLE,
            _ => C_WARN,
        };
        let mut title = vec![Span::from(" ")];
        if let Some(s) = busy {
            title.extend([dot(s), Span::from(" ")]);
        }
        title.push(Span::styled(
            format!("{} ", b.title),
            Style::new().fg(color).bold(),
        ));
        let block = Block::bordered()
            .border_type(if is_selected {
                BorderType::Double
            } else {
                BorderType::Rounded
            })
            .border_style(Style::new().fg(color))
            .title(Line::from(title).centered())
            .padding(Padding::horizontal(1));
        let lines = lines_for(b, b.rect.width.saturating_sub(4) as usize);
        Paragraph::new(lines).block(block).render(clip, &mut buf);
    }
    // Where an arrow leaves a box, notch its border.
    for (x, y, hot, side) in joints {
        if buf.area.contains(Position { x, y }) {
            let cell = &mut buf[(x, y)];
            let sym = match (side, cell.symbol()) {
                (Side::Bottom, "─") => "┬",
                (Side::Bottom, "═") => "╤",
                (Side::Right, "│") => "├",
                (Side::Right, "║") => "╟",
                _ => continue,
            };
            let fg = if hot { C_ACCENT } else { cell.fg };
            cell.set_symbol(sym).set_fg(fg);
        }
    }

    Diagram { buf, selected }
}

#[cfg(test)]
pub(super) mod tests {
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::graph::{Link, LinkSource, ModuleNode};
    use crate::herdr::Agent;
    use crate::modulemap::ModuleMap;

    /// `draw` and `entries` in the default modes, stacked with trunks.
    fn draw(map: &MapInfo, g: &Graph, sel: Option<&Entry>, width: u16) -> Diagram {
        super::draw(map, g, sel, width, Modes::default())
    }

    fn entries<'a>(map: &MapInfo, g: &'a Graph) -> Vec<Entry<'a>> {
        super::entries(map, g, Placement::default())
    }

    pub fn map(toml: &str) -> MapInfo {
        MapInfo::from(&ModuleMap::parse(toml, Path::new("/repo")).unwrap())
    }

    pub fn agent(pane: &str, status: &str) -> AgentNode {
        AgentNode {
            agent: Agent {
                agent: "claude".into(),
                agent_status: status.into(),
                pane_id: pane.into(),
                cwd: PathBuf::from("/repo"),
                title: None,
                terminal_title: None,
            },
            branch: Some("b".into()),
            worktree: None,
            issue: None,
        }
    }

    pub fn issue(number: u64, module: usize, agents: Vec<AgentNode>) -> IssueNode {
        IssueNode {
            number,
            title: format!("Issue {number}"),
            parent: None,
            module: Some(Link {
                target: module,
                source: LinkSource::Classifier,
                confidence: 0.9,
            }),
            agents,
        }
    }

    /// Server depends on search: search's data flows into the server.
    pub const SERVER_READS_SEARCH: &str = "[[module]]\nid = \"server\"\nlabel = \"MCP server\"\n[[module]]\nid = \"search\"\nlabel = \"Search\"\n[[edge]]\nfrom = \"server\"\nto = \"search\"\n";

    /// One agent working on an issue in the server; search idle.
    pub fn busy_server() -> Graph {
        Graph {
            modules: vec![
                ModuleNode {
                    issues: vec![issue(7, 0, vec![agent("p1", "working")])],
                },
                ModuleNode::default(),
            ],
            ..Default::default()
        }
    }

    fn text(buf: &Buffer) -> Vec<String> {
        (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect()
    }

    fn row_with(lines: &[String], needle: &str) -> usize {
        lines
            .iter()
            .position(|l| l.contains(needle))
            .unwrap_or_else(|| panic!("{needle:?} not drawn:\n{}", lines.join("\n")))
    }

    /// The cell where `needle` starts.
    fn find(buf: &Buffer, needle: &str) -> (u16, u16) {
        let lines = text(buf);
        let y = row_with(&lines, needle);
        let x = lines[y][..lines[y].find(needle).unwrap()].chars().count();
        (x as u16, y as u16)
    }

    #[test]
    fn the_arrow_points_from_the_dependency_down_into_the_dependent() {
        let d = draw(&map(SERVER_READS_SEARCH), &busy_server(), None, 80);
        let lines = text(&d.buf);
        let search = row_with(&lines, "SEARCH");
        let server = row_with(&lines, "MCP SERVER");
        assert!(search < server, "{}", lines.join("\n"));
        let head = row_with(&lines, "▼");
        assert_eq!(head, server - 1, "{}", lines.join("\n"));
        assert!(!lines.iter().any(|l| l.contains('▲')));
    }

    #[test]
    fn a_module_with_no_agents_is_still_drawn_grayed_out() {
        let d = draw(&map(SERVER_READS_SEARCH), &busy_server(), None, 80);
        let (x, y) = find(&d.buf, "SEARCH");
        assert_eq!(d.buf[(x, y)].fg, C_IDLE);
        let lines = text(&d.buf);
        assert!(row_with(&lines, "no agents") > y as usize);

        let (x, y) = find(&d.buf, "MCP SERVER");
        assert_eq!(d.buf[(x, y)].fg, C_ACTIVE);
    }

    #[test]
    fn every_module_is_drawn_even_when_no_agent_is_anywhere() {
        let m = map(
            "[[module]]\nid = \"a\"\nlabel = \"Alpha\"\n[[module]]\nid = \"b\"\nlabel = \"Beta\"\n[[module]]\nid = \"c\"\nlabel = \"Gamma\"\n[[edge]]\nfrom = \"b\"\nto = \"a\"\n",
        );
        let g = Graph {
            modules: (0..3).map(|_| ModuleNode::default()).collect(),
            ..Default::default()
        };
        let lines = text(&draw(&m, &g, None, 80).buf);
        for label in ["ALPHA", "BETA", "GAMMA"] {
            row_with(&lines, label);
        }
    }

    #[test]
    fn issues_with_no_module_and_agents_with_no_issue_go_in_the_tray() {
        let mut g = busy_server();
        g.unplaced_issues = vec![IssueNode {
            module: None,
            ..issue(9, 0, vec![agent("p2", "idle")])
        }];
        g.no_issue_agents = vec![agent("p3", "working")];
        let lines = text(&draw(&map(SERVER_READS_SEARCH), &g, None, 80).buf);
        let server = row_with(&lines, "MCP SERVER");
        assert!(row_with(&lines, "NOT PLACED") > server);
        assert!(row_with(&lines, "NO ISSUE") > server);
        row_with(&lines, "unclassified");
    }

    #[test]
    fn the_selected_agents_box_is_reported_and_drawn_double() {
        let m = map(SERVER_READS_SEARCH);
        let g = busy_server();
        let es = entries(&m, &g);
        let d = draw(&m, &g, es.first(), 80);
        let r = d.selected.expect("selected box");
        assert_eq!(d.buf[(r.x, r.y)].symbol(), "╔");
        let (_, y) = find(&d.buf, "MCP SERVER");
        assert_eq!(r.y, y);
    }

    #[test]
    fn a_row_too_wide_for_the_screen_wraps_and_every_module_is_drawn() {
        let ids: Vec<String> = (0..10).map(|i| format!("module{i}")).collect();
        let toml: String = ids
            .iter()
            .map(|id| format!("[[module]]\nid = \"{id}\"\n"))
            .collect();
        let g = Graph {
            modules: (0..10).map(|_| ModuleNode::default()).collect(),
            ..Default::default()
        };
        let d = draw(&map(&toml), &g, None, 80);
        let lines = text(&d.buf);
        for id in &ids {
            row_with(&lines, &format!(" {} ", id.to_uppercase()));
        }
    }

    #[test]
    fn sub_issues_sit_under_their_parent() {
        let mut child = issue(91, 0, vec![agent("p1", "working")]);
        child.parent = Some(crate::world::IssueRef {
            number: 86,
            title: "Cut token use".into(),
        });
        let mut g = busy_server();
        g.modules[0].issues.push(child);
        let lines = text(&draw(&map(SERVER_READS_SEARCH), &g, None, 80).buf);
        let parent = row_with(&lines, "#86 Cut token use");
        assert!(row_with(&lines, "#91 Issue 91") > parent);
        assert!(row_with(&lines, "#7 Issue 7") < parent);
    }

    #[test]
    fn a_narrow_screen_squeezes_boxes_without_panicking() {
        let m = map(
            "[[module]]\nid = \"a\"\n[[module]]\nid = \"b\"\n[[module]]\nid = \"c\"\n[[edge]]\nfrom = \"b\"\nto = \"a\"\n[[edge]]\nfrom = \"c\"\nto = \"a\"\n",
        );
        let g = Graph {
            modules: (0..3).map(|_| ModuleNode::default()).collect(),
            ..Default::default()
        };
        for width in [0, 3, 12, 30] {
            draw(&m, &g, None, width);
        }
    }

    /// A map of modules with labels, and edges as `(from, to)`: `from`
    /// reads `to`, so data flows from `to` into `from`.
    pub fn map_of(modules: &[&str], edges: &[(&str, &str)]) -> MapInfo {
        let mut toml = String::new();
        for m in modules {
            toml.push_str(&format!("[[module]]\nid = \"{m}\"\nlabel = \"{m}\"\n"));
        }
        for (from, to) in edges {
            toml.push_str(&format!("[[edge]]\nfrom = \"{from}\"\nto = \"{to}\"\n"));
        }
        map(&toml)
    }

    fn idle(m: &MapInfo) -> Graph {
        Graph {
            modules: (0..m.labels.len()).map(|_| ModuleNode::default()).collect(),
            ..Default::default()
        }
    }

    const CHAINS: Modes = Modes {
        placement: Placement::Chains,
        arrows: Arrows::Trunks,
    };

    /// a feeds b, b feeds c, c feeds d; a also feeds c and d directly, two
    /// arrows that skip a row.
    fn one_source_two_long_arrows() -> MapInfo {
        map_of(
            &["a", "b", "c", "d"],
            &[("b", "a"), ("c", "b"), ("d", "c"), ("c", "a"), ("d", "a")],
        )
    }

    #[test]
    fn under_chains_two_chained_modules_sit_on_one_line_joined_sideways() {
        let m = map_of(&["src", "dst"], &[("dst", "src")]);
        let d = super::draw(&m, &idle(&m), None, 80, CHAINS);
        let (xa, ya) = find(&d.buf, "SRC");
        let (xb, yb) = find(&d.buf, "DST");
        assert_eq!(ya, yb);
        assert!(xa < xb);
        let lines = text(&d.buf);
        let arrow = &lines[ya as usize + 1];
        assert!(arrow.contains("├───▶"), "{}", lines.join("\n"));
        assert!(!lines.iter().any(|l| l.contains('▼')));
    }

    #[test]
    fn under_chains_a_chain_wider_than_the_screen_wraps_and_every_module_is_drawn() {
        let ids: Vec<String> = (0..8).map(|i| format!("stage{i}")).collect();
        let names: Vec<&str> = ids.iter().map(String::as_str).collect();
        let edges: Vec<(&str, &str)> = names.windows(2).map(|w| (w[1], w[0])).collect();
        let m = map_of(&names, &edges);
        let d = super::draw(&m, &idle(&m), None, 80, CHAINS);
        let lines = text(&d.buf);
        let rows: Vec<usize> = ids
            .iter()
            .map(|id| row_with(&lines, &format!(" {} ", id.to_uppercase())))
            .collect();
        assert!(rows.windows(2).all(|w| w[0] <= w[1]), "{rows:?}");
        assert!(rows[0] < rows[7], "{}", lines.join("\n"));
        assert!(rows.iter().filter(|&&r| r == rows[0]).count() > 1);
    }

    /// Each row's first four columns: more than a margin of two lanes takes.
    fn margin_cols(d: &Diagram) -> Vec<String> {
        text(&d.buf)
            .iter()
            .map(|l| l.chars().take(4).collect())
            .collect()
    }

    #[test]
    fn with_trunks_two_long_arrows_from_one_source_share_one_lane() {
        let m = one_source_two_long_arrows();
        let d = draw(&m, &idle(&m), None, 80);
        let cols = margin_cols(&d);
        assert!(cols.iter().any(|c| c.chars().nth(1) == Some('│')));
        assert!(cols.iter().any(|c| c.chars().nth(1) == Some('├')));
        // One lane: a margin of 2, so no lane runs down column 3.
        assert!(
            cols.iter()
                .all(|c| matches!(c.chars().nth(3), Some(' ' | '─'))),
            "{}",
            text(&d.buf).join("\n")
        );
    }

    #[test]
    fn with_stubs_the_target_names_its_source_and_no_lane_is_drawn() {
        let m = one_source_two_long_arrows();
        let modes = Modes {
            placement: Placement::Stacked,
            arrows: Arrows::Stubs,
        };
        let d = super::draw(&m, &idle(&m), None, 80, modes);
        let lines = text(&d.buf);
        let c = row_with(&lines, " C ");
        assert_eq!(row_with(&lines, "◂ a"), c + 1, "{}", lines.join("\n"));
        assert!(margin_cols(&d).iter().all(|c| c.trim().is_empty()));
    }

    #[test]
    fn selection_order_follows_the_diagram_rows_then_the_tray() {
        let m = map(SERVER_READS_SEARCH);
        let mut g = busy_server();
        g.modules[1].issues = vec![issue(8, 1, vec![agent("p0", "working")])];
        g.no_issue_agents = vec![agent("p9", "idle")];
        let panes: Vec<&str> = entries(&m, &g)
            .iter()
            .map(|e| e.agent.agent.pane_id.as_str())
            .collect();
        // Search is upstream of the server, so its row comes first.
        assert_eq!(panes, ["p0", "p1", "p9"]);
    }
}
