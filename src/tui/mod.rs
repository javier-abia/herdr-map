//! The interactive view: modules drawn as boxes joined by data-flow arrows.
//!
//! The collectors run on a thread of their own and send each tick's graph
//! over a channel; the UI thread only draws and handles keys, so a slow
//! GitHub or OpenRouter call never freezes it.

mod diagram;
mod flow;

use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::graph::{Graph, LinkSource};
use crate::herdr;
pub use diagram::{Arrows, Modes, Placement};
use diagram::{C_ACCENT, C_NUM, Entry, branch, dim, dot, entries};
pub use flow::MapInfo;

/// What one collection pass yields: the graph, errors, warnings.
pub type Tick = Result<(Graph, Vec<String>, Vec<String>)>;

/// Rows PgUp and PgDn move.
const PAGE: u16 = 10;

struct App {
    map: MapInfo,
    graph: Option<Graph>,
    errors: Vec<String>,
    warnings: Vec<String>,
    updated: Option<Instant>,
    /// Index into `entries`.
    selected: usize,
    /// The outcome of the last focus, shown under the diagram.
    status: String,
    /// Rows of the diagram scrolled off the top.
    scroll: u16,
    /// The selection moved since the last frame: scroll its box into view.
    follow: bool,
    /// How the diagram is laid out; `v` and `e` switch it.
    modes: Modes,
}

/// What a key asks the run loop to do.
#[derive(Debug, PartialEq)]
enum Action {
    Quit,
    Focus(String),
}

impl App {
    fn new(map: MapInfo, modes: Modes) -> Self {
        Self {
            map,
            graph: None,
            errors: Vec::new(),
            warnings: Vec::new(),
            updated: None,
            selected: 0,
            status: String::new(),
            scroll: 0,
            follow: true,
            modes,
        }
    }

    fn entries(&self) -> Vec<Entry<'_>> {
        self.graph
            .as_ref()
            .map_or_else(Vec::new, |g| entries(&self.map, g, self.modes.placement))
    }

    fn selected_pane(&self) -> Option<String> {
        let es = self.entries();
        Some(es.get(self.selected)?.agent.agent.pane_id.clone())
    }

    /// Selects the agent in `pane` again, wherever it now is in the order.
    fn reselect(&mut self, pane: Option<String>) {
        let es = self.entries();
        if let Some(i) = es
            .iter()
            .position(|e| Some(&e.agent.agent.pane_id) == pane.as_ref())
        {
            self.selected = i;
        }
        self.follow = true;
    }

    fn update(&mut self, tick: Tick) {
        match tick {
            Ok((graph, errors, warnings)) => {
                self.graph = Some(graph);
                self.errors = errors;
                self.warnings = warnings;
                self.updated = Some(Instant::now());
            }
            // The last graph stays up; only the error is new.
            Err(e) => self.errors = vec![format!("{e:#}")],
        }
        // Agents come and go between ticks.
        let n = self.entries().len();
        self.selected = self.selected.min(n.saturating_sub(1));
    }

    fn on_key(&mut self, key: KeyCode) -> Option<Action> {
        let n = self.entries().len();
        let select = |app: &mut Self, i: usize| {
            app.selected = i;
            app.follow = true;
        };
        match key {
            KeyCode::Char('q') | KeyCode::Esc => return Some(Action::Quit),
            KeyCode::Char('j') | KeyCode::Down => {
                select(self, (self.selected + 1).min(n.saturating_sub(1)))
            }
            KeyCode::Char('k') | KeyCode::Up => select(self, self.selected.saturating_sub(1)),
            // The boxes move: keep the selected agent on screen, and on the
            // same agent, which may now come at another place in the order.
            KeyCode::Char('v') => {
                let pane = self.selected_pane();
                self.modes.placement = self.modes.placement.next();
                self.reselect(pane);
            }
            KeyCode::Char('e') => {
                self.modes.arrows = self.modes.arrows.next();
                self.follow = true;
            }
            KeyCode::PageDown => self.scroll = self.scroll.saturating_add(PAGE),
            KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(PAGE),
            KeyCode::Enter => {
                let pane = self
                    .entries()
                    .get(self.selected)?
                    .agent
                    .agent
                    .pane_id
                    .clone();
                return Some(Action::Focus(pane));
            }
            _ => {}
        }
        None
    }
}

/// Rows scrolled off the top of a diagram `content` rows tall shown in
/// `view` rows: `scroll`, moved to bring `follow` (the selected box) into
/// view when given, and kept within the diagram.
fn scroll_to(scroll: u16, view: u16, content: u16, follow: Option<Rect>) -> u16 {
    let mut scroll = scroll;
    if let Some(r) = follow {
        if r.bottom() > scroll + view {
            scroll = r.bottom() - view;
        }
        // A box taller than the view shows its top.
        if r.y < scroll || r.height > view {
            scroll = r.y.saturating_sub(1);
        }
    }
    scroll.min(content.saturating_sub(view))
}

/// Takes over the terminal until the user quits. `tick` runs on its own
/// thread: it collects one graph and says how long to wait for the next.
pub fn run<F>(map: MapInfo, modes: Modes, mut tick: F) -> Result<()>
where
    F: FnMut() -> (Tick, Duration) + Send + 'static,
{
    let (tx, rx) = mpsc::channel::<Tick>();
    std::thread::spawn(move || {
        loop {
            let (t, wait) = tick();
            if tx.send(t).is_err() {
                return; // the UI has quit
            }
            std::thread::sleep(wait);
        }
    });

    let mut app = App::new(map, modes);
    let mut terminal = ratatui::init();
    let result = (|| -> Result<()> {
        loop {
            while let Ok(t) = rx.try_recv() {
                app.update(t);
            }
            terminal.draw(|f| draw(f, &mut app))?;

            // Wake up now and then to pick up new ticks and the clock.
            if !event::poll(Duration::from_millis(200))? {
                continue;
            }
            let Event::Key(key) = event::read()? else {
                continue;
            };
            if key.kind != KeyEventKind::Press {
                continue;
            }
            match app.on_key(key.code) {
                Some(Action::Quit) => return Ok(()),
                Some(Action::Focus(pane)) => {
                    app.status = match herdr::focus_agent(&pane) {
                        Ok(()) => format!("focused {pane}"),
                        Err(e) => format!("{e:#}"),
                    }
                }
                None => {}
            }
        }
    })();
    ratatui::restore();
    result
}

fn draw(f: &mut Frame, app: &mut App) {
    let notes = app.errors.len() + app.warnings.len() + usize::from(!app.status.is_empty());
    let [header, body, notes_area, bar] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(notes as u16),
        Constraint::Length(1),
    ])
    .areas(f.area());

    f.render_widget(header_line(app), header);
    if app.graph.is_some() {
        draw_body(f, body, app);
    } else {
        f.render_widget(Paragraph::new("  collecting…").style(dim()), body);
    }

    let mut lines: Vec<Line> = app
        .errors
        .iter()
        .map(|e| Line::from(format!(" ✗ {e}")).red())
        .collect();
    lines.extend(
        app.warnings
            .iter()
            .map(|w| Line::from(format!(" ! {w}")).yellow()),
    );
    if !app.status.is_empty() {
        lines.push(Line::from(format!(" » {}", app.status)).cyan());
    }
    f.render_widget(Paragraph::new(lines), notes_area);

    let key = |k: &'static str| Span::styled(k, Style::new().fg(C_ACCENT).bold());
    let txt = |t: &'static str| Span::styled(t, dim());
    f.render_widget(
        Line::from(vec![
            key(" ↑↓"),
            txt(" select   "),
            key("⏎"),
            txt(" focus agent   "),
            key("PgUp/PgDn"),
            txt(" scroll   "),
            key("v"),
            Span::styled(format!(" layout: {}   ", app.modes.placement.name()), dim()),
            key("e"),
            Span::styled(
                format!(" long arrows: {}   ", app.modes.arrows.name()),
                dim(),
            ),
            key("q"),
            txt(" quit"),
        ]),
        bar,
    );
}

/// Project, agent counts, and how fresh the data is.
fn header_line(app: &App) -> Line<'static> {
    let mut spans = vec![
        Span::styled(" ◆ herdr-map ", Style::new().fg(C_ACCENT).bold()),
        Span::from(format!("{} ", app.map.name)).bold(),
    ];
    if let Some(g) = &app.graph {
        let es = app.entries();
        let count = |status: &str| {
            es.iter()
                .filter(|e| e.agent.agent.agent_status == status)
                .count()
        };
        let (working, blocked) = (count("working"), count("blocked"));
        spans.push(Span::from(format!(
            "· {} agent{} ",
            es.len(),
            if es.len() == 1 { "" } else { "s" }
        )));
        spans.push(Span::styled(
            format!("{working} working "),
            Style::new().green(),
        ));
        if blocked > 0 {
            spans.push(Span::styled(
                format!("{blocked} blocked "),
                Style::new().red().bold(),
            ));
        }
        if g.elsewhere > 0 {
            spans.push(Span::styled(format!("· {} elsewhere", g.elsewhere), dim()));
        }
    }
    if let Some(t) = app.updated {
        spans.push(Span::styled(
            format!("   updated {}s ago", t.elapsed().as_secs()),
            dim(),
        ));
    }
    Line::from(spans)
}

/// The diagram, scrolled, and the selected agent in full underneath.
fn draw_body(f: &mut Frame, area: Rect, app: &mut App) {
    let Some(g) = &app.graph else { return };
    let [canvas, info] = Layout::vertical([Constraint::Min(3), Constraint::Length(2)]).areas(area);
    let es = entries(&app.map, g, app.modes.placement);
    let sel = es.get(app.selected);
    let d = diagram::draw(&app.map, g, sel, canvas.width, app.modes);

    let content = d.buf.area.height;
    let follow = if app.follow { d.selected } else { None };
    app.scroll = scroll_to(app.scroll, canvas.height, content, follow);
    app.follow = false;

    // Centred when it fits; otherwise the visible window of it.
    let top = canvas.y + canvas.height.saturating_sub(content) / 2;
    let buf = f.buffer_mut();
    for y in 0..canvas.height.min(content) {
        for x in 0..canvas.width {
            buf[(canvas.x + x, top + y)] = d.buf[(x, y + app.scroll)].clone();
        }
    }
    let more = |text: &'static str, y: u16, buf: &mut ratatui::buffer::Buffer| {
        buf.set_string(
            canvas.right().saturating_sub(9),
            y,
            text,
            Style::new().fg(C_ACCENT),
        );
    };
    if app.scroll > 0 {
        more("▲ more ", canvas.y, buf);
    }
    if app.scroll + canvas.height < content {
        more("▼ more ", canvas.bottom() - 1, buf);
    }

    f.render_widget(Paragraph::new(vec![selected_line(sel)]), info);
}

/// The selected agent in full: status, pane, branch, issue and how the
/// issue was linked, and herdr's title for it.
fn selected_line(sel: Option<&Entry>) -> Line<'static> {
    let mut spans = vec![Span::from(" ")];
    let Some(e) = sel else {
        return Line::from(spans);
    };
    let a = &e.agent.agent;
    spans.extend([
        dot(&a.agent_status),
        Span::from(format!(" {} ", a.agent)).bold(),
        Span::styled(format!("{}  ", a.pane_id), dim()),
        Span::styled(format!("⎇ {}", branch(e.agent)), Style::new().fg(C_NUM)),
    ]);
    if let Some(i) = e.issue {
        let via = match e.agent.issue.as_ref().map(|l| l.source) {
            Some(LinkSource::TitleMatch) => "  (title match)",
            Some(LinkSource::PullRequest) => "  (via PR)",
            Some(LinkSource::AgentTitle) => "  (agent title)",
            _ => "",
        };
        spans.extend([
            Span::styled("  ·  ", dim()),
            Span::styled(format!("#{} ", i.number), Style::new().fg(C_NUM).bold()),
            Span::from(i.title.clone()),
            Span::styled(via, dim()),
        ]);
    }
    if let Some(t) = a.title.as_deref().filter(|t| !t.is_empty()) {
        spans.push(Span::styled(format!("  “{t}”"), dim().italic()));
    }
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::ModuleNode;
    use diagram::tests::{SERVER_READS_SEARCH, agent, busy_server, issue, map, map_of};

    fn app_with(g: Graph) -> App {
        let mut app = App::new(map(SERVER_READS_SEARCH), Modes::default());
        app.update(Ok((g, vec![], vec![])));
        app
    }

    /// Three agents: p0 on search, p1 on the server, p9 with no issue.
    fn three_agents() -> App {
        let mut g = busy_server();
        g.modules[1].issues = vec![issue(8, 1, vec![agent("p0", "working")])];
        g.no_issue_agents = vec![agent("p9", "idle")];
        app_with(g)
    }

    #[test]
    fn arrows_move_the_selection_within_the_agents() {
        let mut app = three_agents();
        app.on_key(KeyCode::Up);
        assert_eq!(app.selected, 0);
        for _ in 0..5 {
            app.on_key(KeyCode::Char('j'));
        }
        assert_eq!(app.selected, 2);
        app.on_key(KeyCode::Char('k'));
        assert_eq!(app.selected, 1);
    }

    #[test]
    fn enter_focuses_the_selected_agents_pane() {
        let mut app = three_agents();
        app.on_key(KeyCode::Down);
        assert_eq!(app.on_key(KeyCode::Enter), Some(Action::Focus("p1".into())));
    }

    #[test]
    fn enter_with_no_agents_does_nothing() {
        let mut app = app_with(Graph {
            modules: vec![Default::default(), Default::default()],
            ..Default::default()
        });
        assert_eq!(app.on_key(KeyCode::Enter), None);
    }

    #[test]
    fn q_and_esc_quit() {
        let mut app = three_agents();
        assert_eq!(app.on_key(KeyCode::Char('q')), Some(Action::Quit));
        assert_eq!(app.on_key(KeyCode::Esc), Some(Action::Quit));
    }

    #[test]
    fn selection_stays_on_an_agent_when_agents_leave() {
        let mut app = three_agents();
        app.selected = 2;
        app.update(Ok((busy_server(), vec![], vec![])));
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn a_failed_tick_keeps_the_last_graph_and_shows_the_error() {
        let mut app = three_agents();
        app.update(Err(anyhow::anyhow!("herdr not running")));
        assert!(app.graph.is_some());
        assert_eq!(app.errors, ["herdr not running"]);
    }

    #[test]
    fn page_keys_scroll_and_scrolling_stays_within_the_diagram() {
        let mut app = three_agents();
        app.on_key(KeyCode::PageDown);
        app.on_key(KeyCode::PageDown);
        assert_eq!(app.scroll, 2 * PAGE);
        assert_eq!(scroll_to(app.scroll, 10, 25, None), 15);
        assert_eq!(scroll_to(app.scroll, 30, 25, None), 0);
        app.on_key(KeyCode::PageUp);
        app.on_key(KeyCode::PageUp);
        app.on_key(KeyCode::PageUp);
        assert_eq!(app.scroll, 0);
    }

    #[test]
    fn scrolling_brings_the_selected_box_into_view() {
        let below = Rect::new(0, 30, 20, 5);
        assert_eq!(scroll_to(0, 10, 50, Some(below)), 25);
        let above = Rect::new(0, 4, 20, 5);
        assert_eq!(scroll_to(25, 10, 50, Some(above)), 3);
        let visible = Rect::new(0, 12, 20, 5);
        assert_eq!(scroll_to(10, 10, 50, Some(visible)), 10);
        // Taller than the view: its top is shown.
        let tall = Rect::new(0, 20, 20, 15);
        assert_eq!(scroll_to(0, 10, 50, Some(tall)), 19);
    }

    #[test]
    fn v_and_e_switch_the_placement_and_the_arrow_style() {
        let mut app = three_agents();
        assert_eq!(app.modes, Modes::default());
        app.on_key(KeyCode::Char('v'));
        assert_eq!(app.modes.placement, Placement::Chains);
        app.on_key(KeyCode::Char('v'));
        assert_eq!(app.modes.placement, Placement::Stacked);
        app.on_key(KeyCode::Char('e'));
        assert_eq!(app.modes.arrows, Arrows::Stubs);
        app.on_key(KeyCode::Char('e'));
        assert_eq!(app.modes.arrows, Arrows::Trunks);
    }

    /// a → b → c is a chain; x is fed by a alone and sits in b's layer.
    /// One agent on each module: pa, pb, pc, px.
    fn chain_with_a_side_module() -> App {
        let m = map_of(&["a", "b", "c", "x"], &[("b", "a"), ("c", "b"), ("x", "a")]);
        let g = Graph {
            modules: ["pa", "pb", "pc", "px"]
                .iter()
                .enumerate()
                .map(|(i, p)| ModuleNode {
                    issues: vec![issue(i as u64 + 1, i, vec![agent(p, "working")])],
                })
                .collect(),
            ..Default::default()
        };
        let mut app = App::new(m, Modes::default());
        app.update(Ok((g, vec![], vec![])));
        app
    }

    fn panes(app: &App) -> Vec<String> {
        app.entries()
            .iter()
            .map(|e| e.agent.agent.pane_id.clone())
            .collect()
    }

    #[test]
    fn under_chains_selection_follows_the_lines() {
        let mut app = chain_with_a_side_module();
        assert_eq!(panes(&app), ["pa", "pb", "px", "pc"]);
        app.on_key(KeyCode::Char('v'));
        assert_eq!(panes(&app), ["pa", "pb", "pc", "px"]);
    }

    #[test]
    fn switching_the_placement_keeps_the_same_agent_selected() {
        let mut app = chain_with_a_side_module();
        app.selected = 3; // pc
        app.on_key(KeyCode::Char('v'));
        assert_eq!(app.selected, 2);
        assert!(app.follow);
    }

    #[test]
    fn moving_the_selection_follows_it_but_paging_does_not() {
        let mut app = three_agents();
        app.follow = false;
        app.on_key(KeyCode::PageDown);
        assert!(!app.follow);
        app.on_key(KeyCode::Down);
        assert!(app.follow);
    }
}
