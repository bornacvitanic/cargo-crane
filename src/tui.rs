//! Interactive browser: every module in the workspace on the left, its live
//! extraction plan on the right, `Enter`/`a` to lift the selected one. Same
//! header, palette, and keys as the rest of the freight suite.

use std::io::{self, Stdout};
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::{Frame, Terminal};

use crate::plan::{self, Plan};
use crate::workspace::Workspace;
use crate::{analyze, apply, closure};

const ACCENT: Color = Color::Cyan;
const MUTED: Color = Color::DarkGray;
const TICK: Duration = Duration::from_millis(100);

type Term = Terminal<CrosstermBackend<Stdout>>;

struct Entry {
    pkg: String,
    module: String,
}

struct App<'a> {
    ws: &'a Workspace,
    allow_dirty: bool,
    entries: Vec<Entry>,
    state: ListState,
    /// Cached plan (or analysis error) for the current selection.
    plan: Option<Result<Plan, String>>,
    plan_for: Option<usize>,
    status: String,
    quit: bool,
    done: Option<Vec<String>>,
}

impl<'a> App<'a> {
    fn new(ws: &'a Workspace, allow_dirty: bool) -> Self {
        let mut entries = Vec::new();
        for pkg in &ws.packages {
            let mut mods: Vec<String> = analyze::top_level_modules(pkg).into_iter().collect();
            mods.sort();
            for module in mods {
                entries.push(Entry {
                    pkg: pkg.name.clone(),
                    module,
                });
            }
        }
        let mut state = ListState::default();
        if !entries.is_empty() {
            state.select(Some(0));
        }
        Self {
            ws,
            allow_dirty,
            entries,
            state,
            plan: None,
            plan_for: None,
            status: String::new(),
            quit: false,
            done: None,
        }
    }

    fn move_sel(&mut self, delta: isize) {
        if self.entries.is_empty() {
            return;
        }
        let len = self.entries.len() as isize;
        let cur = self.state.selected().unwrap_or(0) as isize;
        self.state
            .select(Some((cur + delta).rem_euclid(len) as usize));
        self.status.clear();
    }

    /// Compute the plan for the current selection if it isn't cached yet.
    fn ensure_plan(&mut self) {
        let Some(sel) = self.state.selected() else {
            self.plan = None;
            return;
        };
        if self.plan_for == Some(sel) {
            return;
        }
        let entry = &self.entries[sel];
        let result = match self.ws.find(&entry.pkg) {
            Some(pkg) => closure::compute(pkg, &entry.module).map(|c| plan::build(self.ws, pkg, c)),
            None => Err(format!("package `{}` not found", entry.pkg)),
        };
        self.plan = Some(result);
        self.plan_for = Some(sel);
    }

    fn apply_selected(&mut self) {
        let Some(sel) = self.state.selected() else {
            return;
        };
        let plan = match &self.plan {
            Some(Ok(p)) => p,
            Some(Err(_)) => {
                self.status = "cannot apply: analysis failed".into();
                return;
            }
            None => return,
        };
        if !plan.closure.extractable() {
            self.status = "blocked — this module can't be lifted as-is".into();
            return;
        }
        let pkg = self
            .ws
            .find(&self.entries[sel].pkg)
            .expect("selected entry's package exists");
        match apply::apply(self.ws, pkg, plan, self.allow_dirty) {
            Ok(log) => {
                self.done = Some(log);
                self.quit = true;
            }
            Err(err) => self.status = err,
        }
    }
}

pub fn run(ws: &Workspace, allow_dirty: bool) -> Result<(), String> {
    let mut app = App::new(ws, allow_dirty);

    enable_raw_mode().map_err(|e| e.to_string())?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen).map_err(|e| e.to_string())?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout)).map_err(|e| e.to_string())?;

    let loop_result = event_loop(&mut terminal, &mut app);

    disable_raw_mode().ok();
    execute!(terminal.backend_mut(), LeaveAlternateScreen).ok();
    terminal.show_cursor().ok();
    loop_result.map_err(|e| e.to_string())?;

    if let Some(log) = &app.done {
        println!("extraction complete:");
        for line in log {
            println!("  ✓ {line}");
        }
        println!("\nnext: run `cargo check` to verify the workspace still builds.");
    }
    Ok(())
}

fn event_loop(terminal: &mut Term, app: &mut App) -> io::Result<()> {
    while !app.quit {
        app.ensure_plan();
        terminal.draw(|frame| draw(frame, app))?;
        if event::poll(TICK)? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    match key.code {
                        KeyCode::Char('q') | KeyCode::Esc => app.quit = true,
                        KeyCode::Up | KeyCode::Char('k') => app.move_sel(-1),
                        KeyCode::Down | KeyCode::Char('j') => app.move_sel(1),
                        KeyCode::Enter | KeyCode::Char('a') => app.apply_selected(),
                        _ => {}
                    }
                }
            }
        }
    }
    Ok(())
}

fn draw(frame: &mut Frame, app: &mut App) {
    let rows = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(3),
    ])
    .split(frame.area());

    let header = Line::from(vec![
        Span::styled(
            "⚓ freight",
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(" · cargo-crane   ({} modules)", app.entries.len()),
            Style::default().fg(MUTED),
        ),
    ]);
    frame.render_widget(Paragraph::new(header), rows[0]);

    let body =
        Layout::horizontal([Constraint::Percentage(42), Constraint::Percentage(58)]).split(rows[1]);

    let items: Vec<ListItem> = app
        .entries
        .iter()
        .map(|e| ListItem::new(format!("{}::{}", e.pkg, e.module)))
        .collect();
    let list = List::new(items)
        .block(Block::bordered().title(" modules "))
        .highlight_style(Style::default().fg(ACCENT).add_modifier(Modifier::REVERSED))
        .highlight_symbol("> ");
    frame.render_stateful_widget(list, body[0], &mut app.state);

    frame.render_widget(detail(app), body[1]);
    frame.render_widget(footer(app), rows[2]);
}

fn detail(app: &App) -> Paragraph<'static> {
    let lines: Vec<Line> = match &app.plan {
        Some(Ok(plan)) => plan.lines().into_iter().map(styled_line).collect(),
        Some(Err(e)) => vec![Line::from(Span::styled(
            format!("analysis failed: {e}"),
            Style::default().fg(Color::Red),
        ))],
        None => vec![Line::from("no modules to extract in this workspace")],
    };
    Paragraph::new(lines)
        .block(Block::bordered().title(" extraction plan "))
        .wrap(Wrap { trim: false })
}

/// Colour the verdict and coupling lines; leave the rest plain.
fn styled_line(s: String) -> Line<'static> {
    let style = if s.starts_with("verdict: ready") {
        Style::default().fg(Color::Green)
    } else if s.starts_with("verdict: blocked") {
        Style::default().fg(Color::Red)
    } else if s.contains('⚠') {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default()
    };
    Line::from(Span::styled(s, style))
}

fn footer(app: &App) -> Paragraph<'static> {
    let hint = |key: &'static str, label: &'static str| {
        [
            Span::styled(key, Style::default().fg(ACCENT)),
            Span::raw(format!(" {label}  ")),
        ]
    };
    let mut spans: Vec<Span> = [
        hint("↑/↓ · j/k", "move"),
        hint("Enter/a", "lift"),
        hint("q", "quit"),
    ]
    .concat();
    if !app.status.is_empty() {
        spans.push(Span::styled(
            app.status.clone(),
            Style::default().fg(Color::Yellow),
        ));
    }
    Paragraph::new(Line::from(spans)).block(Block::bordered())
}
