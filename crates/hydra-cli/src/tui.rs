//! Full-screen interactive download manager.
//!
//! Hand-rolled on `crossterm` rather than a widget framework. The reason is the
//! same one that shaped `progress.rs`: the state worth showing is per-connection,
//! and a generic table widget flattens it. It also keeps the dependency surface
//! small enough that this builds offline.
//!
//! All queue behaviour lives in [`crate::queue`] and is unit-tested there. This
//! module is only input handling and drawing, so a rendering change cannot break
//! scheduling.
//!
//! # Terminal state is restored on every exit path
//!
//! Raw mode and the alternate screen are process-global terminal state. Leaving
//! them set because a transfer panicked hands the user a shell with no echo and no
//! line editing, which they then have to fix with `reset`. The guard below restores
//! on drop, so panics and `?` returns both clean up.

use crate::queue::{EventLog, Queue, State};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use crossterm::{cursor, execute, terminal};
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// Restores terminal state on drop, including during a panic.
struct TerminalGuard;

impl TerminalGuard {
    fn enter() -> io::Result<Self> {
        terminal::enable_raw_mode()?;
        execute!(io::stdout(), terminal::EnterAlternateScreen, cursor::Hide)?;
        Ok(Self)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = execute!(io::stdout(), cursor::Show, terminal::LeaveAlternateScreen);
        let _ = terminal::disable_raw_mode();
    }
}

/// What the user asked for on this tick.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    /// Quit and stop every running transfer.
    Quit,
    /// Leave the UI but let running transfers continue in the background.
    ///
    /// Distinct from Quit because the two are opposite intentions and sharing one key
    /// for both is how people lose a 4 GB download: `q` cancels, `b` detaches. The queue
    /// file is the handoff — a later session reads it and reattaches.
    Background,
    Pause(u64),
    Resume(u64),
    Cancel(u64),
    MoveUp(u64),
    MoveDown(u64),
    ClearFinished,
    /// Adjust how many transfers may run at once.
    Concurrency(isize),
    /// Open the per-connection detail screen for an item.
    OpenDetail(u64),
    /// Return to the list.
    CloseDetail,
    Add(String),
    PluginLoad,
    PluginToggle(String, bool),
    PluginMove(String, isize),
    PluginInstall(String),
    PluginConfirm,
    PluginSettings(String),
    PluginPermission(String, String),
    FormSubmit,
    FormCancel,
    None,
}

/// Which pane has focus / what the UI is doing.
#[derive(Clone, PartialEq, Eq, Debug)]
/// Which screen is showing.
///
/// A detail view exists because the list row cannot answer the question a stalled
/// download raises: *which* mirror is slow, which byte range is stuck, how many repairs
/// have fired. That state is per-connection and there is no room for it in a single
/// line, so it gets its own screen rather than a wider table.
pub enum Mode {
    List,
    /// Typing a URL to add.
    Adding(String),
    /// Per-connection detail for one queue item.
    Detail,
    Help,
    Plugins,
    PluginPath(String),
    PluginConsent,
    PluginGrant(String, String),
    Form,
}

pub struct Ui {
    /// Live per-connection state, by queue id, as reported by running transfers.
    pub live: std::collections::HashMap<u64, crate::download::Tick>,
    /// The item whose detail screen is showing, if any.
    pub detail: Option<u64>,
    pub selected: usize,
    pub mode: Mode,
    pub log: EventLog,
    pub plugins: Vec<hya_plugin::manager::Installed>,
    pub plugin_selected: usize,
    pub plugin_review: Option<(std::path::PathBuf, hya_plugin_api::Manifest)>,
    pub plugin_prompt: Option<crate::plugin_ui::Prompt>,
    /// Sparkline of aggregate rate.
    history: Vec<f64>,
}

impl Default for Ui {
    fn default() -> Self {
        Self::new()
    }
}

impl Ui {
    pub fn new() -> Self {
        Self {
            live: std::collections::HashMap::new(),
            detail: None,
            selected: 0,
            mode: Mode::List,
            log: EventLog::new(256),
            history: Vec::new(),
            plugins: Vec::new(),
            plugin_selected: 0,
            plugin_review: None,
            plugin_prompt: None,
        }
    }

    /// Map a key to a command against the current queue.
    ///
    /// Pure: takes the queue read-only and returns a command, so every binding is
    /// testable without a terminal.
    pub fn on_key(&mut self, k: KeyEvent, q: &Queue) -> Command {
        // Ctrl-C quits from anywhere, including mid-typing.
        if k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('c') {
            return Command::Quit;
        }
        match &mut self.mode {
            Mode::Form => {
                let Some(prompt) = &mut self.plugin_prompt else {
                    self.mode = Mode::List;
                    return Command::None;
                };
                match k.code {
                    KeyCode::Esc => Command::FormCancel,
                    KeyCode::Enter => Command::FormSubmit,
                    KeyCode::Tab | KeyCode::Down => {
                        prompt.selected = (prompt.selected + 1) % prompt.form.fields.len().max(1);
                        Command::None
                    }
                    KeyCode::Up => {
                        prompt.selected = prompt.selected.saturating_sub(1);
                        Command::None
                    }
                    KeyCode::Char(c) => {
                        if let Some(field) = prompt.form.fields.get(prompt.selected) {
                            prompt.values.entry(field.key.clone()).or_default().push(c);
                        }
                        Command::None
                    }
                    KeyCode::Backspace => {
                        if let Some(field) = prompt.form.fields.get(prompt.selected) {
                            prompt.values.entry(field.key.clone()).or_default().pop();
                        }
                        Command::None
                    }
                    _ => Command::None,
                }
            }
            Mode::PluginGrant(id, capability) => match k.code {
                KeyCode::Esc => {
                    self.mode = Mode::Plugins;
                    Command::None
                }
                KeyCode::Enter => {
                    let operation = Command::PluginPermission(id.clone(), capability.clone());
                    self.mode = Mode::Plugins;
                    operation
                }
                KeyCode::Char(c) => {
                    capability.push(c);
                    Command::None
                }
                KeyCode::Backspace => {
                    capability.pop();
                    Command::None
                }
                _ => Command::None,
            },
            Mode::PluginConsent => match k.code {
                KeyCode::Char('y') => Command::PluginConfirm,
                KeyCode::Esc | KeyCode::Char('n') => {
                    self.mode = Mode::Plugins;
                    self.plugin_review = None;
                    Command::None
                }
                _ => Command::None,
            },
            Mode::PluginPath(path) => match k.code {
                KeyCode::Esc => {
                    self.mode = Mode::Plugins;
                    Command::None
                }
                KeyCode::Enter => {
                    let path = path.clone();
                    self.mode = Mode::Plugins;
                    Command::PluginInstall(path)
                }
                KeyCode::Char(c) => {
                    path.push(c);
                    Command::None
                }
                KeyCode::Backspace => {
                    path.pop();
                    Command::None
                }
                _ => Command::None,
            },
            Mode::Plugins => match k.code {
                KeyCode::Esc => {
                    self.mode = Mode::List;
                    Command::None
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    self.plugin_selected =
                        (self.plugin_selected + 1).min(self.plugins.len().saturating_sub(1));
                    Command::None
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    self.plugin_selected = self.plugin_selected.saturating_sub(1);
                    Command::None
                }
                KeyCode::Char('s') => self
                    .plugins
                    .get(self.plugin_selected)
                    .map(|p| Command::PluginSettings(p.manifest.id.clone()))
                    .unwrap_or(Command::None),
                KeyCode::Char('g') => {
                    if let Some(p) = self.plugins.get(self.plugin_selected) {
                        self.mode = Mode::PluginGrant(p.manifest.id.clone(), String::new());
                    }
                    Command::None
                }
                KeyCode::Char('e') => self
                    .plugins
                    .get(self.plugin_selected)
                    .map(|p| Command::PluginToggle(p.manifest.id.clone(), !p.enabled))
                    .unwrap_or(Command::None),
                KeyCode::Char('J') | KeyCode::Char('K') => self
                    .plugins
                    .get(self.plugin_selected)
                    .map(|p| {
                        Command::PluginMove(
                            p.manifest.id.clone(),
                            if k.code == KeyCode::Char('J') { 1 } else { -1 },
                        )
                    })
                    .unwrap_or(Command::None),
                KeyCode::Char('i') => {
                    self.mode = Mode::PluginPath(String::new());
                    Command::None
                }
                _ => Command::None,
            },
            Mode::Adding(buf) => match k.code {
                KeyCode::Esc => {
                    self.mode = Mode::List;
                    Command::None
                }
                KeyCode::Enter => {
                    let url = buf.trim().to_string();
                    self.mode = Mode::List;
                    if url.is_empty() {
                        Command::None
                    } else {
                        Command::Add(url)
                    }
                }
                KeyCode::Backspace => {
                    buf.pop();
                    Command::None
                }
                KeyCode::Char(c) => {
                    buf.push(c);
                    Command::None
                }
                _ => Command::None,
            },
            Mode::Help => {
                self.mode = Mode::List;
                Command::None
            }
            // Detail view: Esc (or q) returns to the list, and the item-level actions
            // still work so a stalled download can be paused without going back first.
            Mode::Detail => {
                let sel = self.detail;
                match k.code {
                    KeyCode::Esc | KeyCode::Char('q') | KeyCode::Enter => {
                        self.mode = Mode::List;
                        self.detail = None;
                        Command::CloseDetail
                    }
                    KeyCode::Char('p') => sel.map(Command::Pause).unwrap_or(Command::None),
                    KeyCode::Char('r') => sel.map(Command::Resume).unwrap_or(Command::None),
                    KeyCode::Char('d') => sel.map(Command::Cancel).unwrap_or(Command::None),
                    KeyCode::Char('?') | KeyCode::Char('h') => {
                        self.mode = Mode::Help;
                        Command::None
                    }
                    _ => Command::None,
                }
            }
            Mode::List => {
                let sel = q.items.get(self.selected).map(|i| i.id);
                match k.code {
                    // `q` stops everything; `b` detaches and lets transfers continue.
                    // Esc is NOT a quit key here: it is the "go back" key everywhere
                    // else in this UI, and making it also mean "cancel all downloads"
                    // is how someone loses a transfer by reflex.
                    KeyCode::Char('P') => {
                        self.mode = Mode::Plugins;
                        Command::PluginLoad
                    }
                    KeyCode::Char('q') => Command::Quit,
                    KeyCode::Char('b') => Command::Background,
                    KeyCode::Enter => sel.map(Command::OpenDetail).unwrap_or(Command::None),
                    KeyCode::Char('?') | KeyCode::Char('h') => {
                        self.mode = Mode::Help;
                        Command::None
                    }
                    KeyCode::Char('a') => {
                        self.mode = Mode::Adding(String::new());
                        Command::None
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        if !q.items.is_empty() {
                            self.selected = (self.selected + 1).min(q.items.len() - 1);
                        }
                        Command::None
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        self.selected = self.selected.saturating_sub(1);
                        Command::None
                    }
                    KeyCode::Char('p') => sel.map(Command::Pause).unwrap_or(Command::None),
                    KeyCode::Char('r') => sel.map(Command::Resume).unwrap_or(Command::None),
                    KeyCode::Char('d') | KeyCode::Delete => {
                        sel.map(Command::Cancel).unwrap_or(Command::None)
                    }
                    KeyCode::Char('K') => sel.map(Command::MoveUp).unwrap_or(Command::None),
                    KeyCode::Char('J') => sel.map(Command::MoveDown).unwrap_or(Command::None),
                    KeyCode::Char('c') => Command::ClearFinished,
                    KeyCode::Char('+') | KeyCode::Char('=') => Command::Concurrency(1),
                    KeyCode::Right | KeyCode::Char('l') => {
                        sel.map(Command::OpenDetail).unwrap_or(Command::None)
                    }
                    KeyCode::Char('-') => Command::Concurrency(-1),
                    _ => Command::None,
                }
            }
        }
    }

    /// Keep the selection inside the list after items are removed.
    pub fn clamp_selection(&mut self, q: &Queue) {
        if q.items.is_empty() {
            self.selected = 0;
        } else if self.selected >= q.items.len() {
            self.selected = q.items.len() - 1;
        }
    }

    /// Items to draw so the list never exceeds `budget` screen lines and the
    /// selection stays on screen. A failed item spends a second line on its error.
    fn visible_window(&self, q: &Queue, budget: usize) -> std::ops::Range<usize> {
        let lines = |i: usize| 1 + usize::from(q.items[i].error.is_some());
        if q.items.is_empty() {
            return 0..0;
        }
        let selected = self.selected.min(q.items.len() - 1);
        let mut start = 0;
        let mut used: usize = (0..=selected).map(lines).sum();
        while used > budget && start < selected {
            used -= lines(start);
            start += 1;
        }
        let mut end = selected + 1;
        while end < q.items.len() && used + lines(end) <= budget {
            used += lines(end);
            end += 1;
        }
        start..end
    }

    pub fn note_rate(&mut self, rate: f64) {
        self.history.push(rate);
        if self.history.len() > 64 {
            self.history.remove(0);
        }
    }

    /// Render the whole screen into a string of ANSI, so drawing is testable.
    /// Per-connection detail for one item.
    ///
    /// Answers the question the list cannot: which mirror is slow, which byte range is
    /// stuck, how much has actually arrived. Falls back to a clear message when the item
    /// is not running, rather than drawing an empty table that looks broken.
    pub fn render_detail(&self, q: &Queue, id: u64, cols: u16, _rows: u16) -> String {
        let w = cols.max(60) as usize;
        let mut o = String::new();
        let Some(item) = q.get(id) else {
            return "  that item is gone — press Esc\r\n".into();
        };
        o.push_str("\x1b[H\x1b[2J");
        o.push_str(&format!(
            "\x1b[1m {}\x1b[0m  \x1b[90m#{} {}\x1b[0m\r\n",
            item.name(),
            item.id,
            item.state.as_str()
        ));
        o.push_str(&format!(" \x1b[90m{}\x1b[0m\r\n", "─".repeat(w.min(150))));

        let live = self.live.get(&id);
        let done = live.map(|t| t.done).unwrap_or(item.done_bytes);
        let size = live.and_then(|t| t.size).or(item.size);
        match size {
            Some(sz) if sz > 0 => {
                let frac = done as f64 / sz as f64;
                let bar_w = (w.saturating_sub(34)).clamp(10, 60);
                let filled = ((frac * bar_w as f64).round() as usize).min(bar_w);
                o.push_str(&format!(
                    "  \x1b[36m{}\x1b[0m{}  {:>5.1}%  {} / {}\r\n",
                    "━".repeat(filled),
                    "─".repeat(bar_w - filled),
                    frac * 100.0,
                    hya_core::fmt::bytes(done),
                    hya_core::fmt::bytes(sz)
                ));
            }
            _ => o.push_str(&format!(
                "  {} downloaded (total size unknown)\r\n",
                hya_core::fmt::bytes(done)
            )),
        }
        if let Some(t) = live {
            o.push_str(&format!(
                "  {}/s aggregate   {} request(s)   {} repair(s)\r\n",
                hya_core::fmt::bytes(t.rate as u64),
                t.requests,
                t.repairs
            ));
        }
        o.push_str("\r\n");

        match live {
            Some(t) if !t.conns.is_empty() => {
                o.push_str("  \x1b[90mconn  source                     range                    rate      health\x1b[0m\r\n");
                for (i, c) in t.conns.iter().enumerate() {
                    let span = c.hi.saturating_sub(c.lo);
                    let got = c.pos.saturating_sub(c.lo);
                    let mini = if span > 0 {
                        let cells = 10usize;
                        let f = ((got as f64 / span as f64) * cells as f64).round() as usize;
                        format!(
                            "[{}{}]",
                            "▪".repeat(f.min(cells)),
                            "·".repeat(cells - f.min(cells))
                        )
                    } else {
                        "[----------]".into()
                    };
                    let colour = match c.health.as_str() {
                        "healthy" => "\x1b[32m",
                        "suspect" => "\x1b[33m",
                        "degraded" => "\x1b[31m",
                        "stalled" => "\x1b[35m",
                        _ => "\x1b[90m",
                    };
                    let host: String = c.host.chars().take(24).collect();
                    o.push_str(&format!(
                        "  #{i:<4} {host:<24} {mini} {:>9}-{:<9} {:>9}/s  {colour}{}\x1b[0m\r\n",
                        c.lo,
                        c.hi,
                        hya_core::fmt::bytes(c.rate as u64),
                        c.health
                    ));
                }
            }
            _ => o.push_str("  no live connection detail (the item is not transferring)\r\n"),
        }
        if let Some(e) = &item.error {
            o.push_str(&format!("\r\n  \x1b[31m{e}\x1b[0m\r\n"));
        }
        o.push_str(&format!(
            "\r\n \x1b[90m{}\x1b[0m\r\n",
            "─".repeat(w.min(150))
        ));
        o.push_str("  \x1b[90mEsc back   p pause   r resume   d cancel   ? help\x1b[0m\r\n");
        o
    }

    pub fn render(&self, q: &Queue, cols: u16, rows: u16) -> String {
        use std::fmt::Write as _;
        let w = cols.max(40) as usize;
        let mut s = String::new();
        let _ = write!(s, "\x1b[H\x1b[2J");

        // ---- header ----
        let (queued, running, done, failed) = q.counts();
        let _ = writeln!(
            s,
            "\x1b[1;36m hydra\x1b[0m  \x1b[90mfile retriever\x1b[0m{:>width$}\r",
            format!(
                "{} running  {} queued  {} done  {} failed  |  {}/s  |  max {}",
                running,
                queued,
                done,
                failed,
                hya_core::fmt::bytes(q.total_rate() as u64),
                q.max_active
            ),
            width = w.saturating_sub(24)
        );
        let _ = writeln!(s, "\x1b[90m{}\x1b[0m\r", "─".repeat(w));

        if self.mode == Mode::Help {
            for line in [
                "  Keys",
                "",
                "    Enter     open per-connection detail for the selected item",
                "    Esc       leave the detail screen (never quits)",
                "    a         add a URL",
                "    j / k     move selection down / up",
                "    p / r     pause / resume the selected transfer",
                "    d         cancel the selected transfer",
                "    J / K     move the selected item down / up in the queue",
                "    c         clear finished and cancelled items",
                "    + / -     how many transfers run AT ONCE (queue parallelism)",
                "    ? or h    this help",
                "    b         background: leave the UI, transfers keep running",
                "    q         quit: stop the running transfers and exit",
                "",
                "  + / - is queue parallelism, not connection count",
                "",
                "    It sets how many QUEUED ITEMS may transfer simultaneously (1-16).",
                "    Each item still opens as many CONNECTIONS as the scheduler measures",
                "    to be useful for its own mirrors, which is a separate number chosen",
                "    per path: on a link already saturated by one connection, more",
                "    connections add no speed, so the scheduler declines to open them.",
                "    Raising this multiplies total connections; on a shared or metered",
                "    link that is the setting to lower first.",
                "",
                "  b and q are different on purpose",
                "",
                "    b detaches and leaves transfers running, recording the queue so a",
                "    later session reattaches. q stops them. One key for both is how a",
                "    large download gets lost by reflex, so Esc is never a quit key.",
                "",
                "  Paused and failed transfers keep their bytes; resuming continues",
                "  from where they stopped, provided the server still offers the same",
                "  validator. Without one, resume is refused rather than risking a",
                "  file spliced from two different versions of the object.",
                "",
                "  press any key to return",
            ] {
                let _ = writeln!(s, "{line}\r");
            }
            return s;
        }

        // ---- list ----
        let list_rows = (rows as usize).saturating_sub(10).max(3);
        if q.items.is_empty() {
            let _ = writeln!(
                s,
                "\r\n  \x1b[90mthe queue is empty — press 'a' to add a URL\x1b[0m\r"
            );
        }
        let window = self.visible_window(q, list_rows);
        for (idx, it) in q
            .items
            .iter()
            .enumerate()
            .take(window.end)
            .skip(window.start)
        {
            let sel = idx == self.selected;
            let marker = if sel { "\x1b[7m▸" } else { " " };
            let (tag, colour) = match it.state {
                State::Running => ("run ", "\x1b[32m"),
                State::Queued => ("wait", "\x1b[36m"),
                State::Paused => ("hold", "\x1b[33m"),
                State::Done => ("done", "\x1b[92m"),
                State::Failed => ("fail", "\x1b[31m"),
                State::Cancelled => ("gone", "\x1b[90m"),
            };
            let bar_w = 22usize;
            let bar = match it.fraction() {
                Some(fr) => {
                    let k = (fr * bar_w as f64).round() as usize;
                    format!("{}{}", "━".repeat(k), "─".repeat(bar_w - k))
                }
                None => "?".repeat(bar_w),
            };
            let pct = it
                .fraction()
                .map(|f| format!("{:5.1}%", 100.0 * f))
                .unwrap_or_else(|| "    ?".into());
            let size = it
                .size
                .map(hya_core::fmt::bytes)
                .unwrap_or_else(|| "?".into());
            let _ = writeln!(
                s,
                "{marker} {colour}{tag}\x1b[0m {:<28} {bar} {pct} {:>10}/{:<10} {:>10}/s\x1b[0m\r",
                trunc(&it.name(), 28),
                hya_core::fmt::bytes(it.done_bytes),
                size,
                hya_core::fmt::bytes(it.rate as u64)
            );
            if let Some(e) = &it.error {
                let _ = writeln!(
                    s,
                    "      \x1b[31m{}\x1b[0m\r",
                    trunc(e, w.saturating_sub(8))
                );
            }
        }

        // ---- footer ----
        let _ = writeln!(s, "\x1b[90m{}\x1b[0m\r", "─".repeat(w));
        for line in self.log.recent(3) {
            let _ = writeln!(s, "  \x1b[90m{}\x1b[0m\r", trunc(line, w.saturating_sub(4)));
        }
        match &self.mode {
            Mode::Adding(buf) => {
                let _ = writeln!(s, "\r\n  add URL: \x1b[4m{buf}\x1b[0m▏   \x1b[90m(enter to add, esc to cancel)\x1b[0m\r");
            }
            _ => {
                let _ = writeln!(
                    s,
                    "\r\n  \x1b[90mEnter\x1b[0m detail  \x1b[90ma\x1b[0m add  \x1b[90mp\x1b[0m pause  \
                     \x1b[90mP\x1b[0m plugins  \x1b[90mr\x1b[0m resume  \x1b[90md\x1b[0m cancel  \x1b[90mJ/K\x1b[0m reorder  \
                     \x1b[90mc\x1b[0m clear  \x1b[90m+/-\x1b[0m parallel jobs  \x1b[90m?\x1b[0m help  \
                     \x1b[90mb\x1b[0m background  \x1b[90mq\x1b[0m quit\r"
                );
            }
        }
        s
    }
}

fn trunc(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let keep: String = s.chars().take(n.saturating_sub(1)).collect();
        format!("{keep}…")
    }
}

/// Render a representative queue screen to stdout, for documentation and for
/// reviewing the layout without a terminal.
///
/// Drives the real renderer, so what appears here is what a user sees.
pub fn demo_screen() {
    let mut q = Queue::new(3);
    q.add(
        vec!["http://mirror.example.org/ubuntu-24.04.2-desktop-amd64.iso".into()],
        "ubuntu-24.04.2-desktop-amd64.iso".into(),
    );
    q.add(
        vec!["http://mirror.example.org/dataset-2026.tar.zst".into()],
        "dataset-2026.tar.zst".into(),
    );
    q.add(
        vec!["http://cdn.example.net/model-weights.safetensors".into()],
        "model-weights.safetensors".into(),
    );
    q.add(
        vec!["http://cdn.example.net/lecture-03.mkv".into()],
        "lecture-03.mkv".into(),
    );
    q.add(
        vec!["http://broken.example.net/missing.bin".into()],
        "missing.bin".into(),
    );
    q.mark_running(1);
    q.progress(1, 2_950_000_000, Some(6_203_355_136), 24.4e6);
    q.mark_running(2);
    q.progress(2, 411_000_000, Some(890_000_000), 11.2e6);
    q.mark_running(3);
    q.progress(3, 96_000_000, Some(4_100_000_000), 1.1e6);
    q.pause(4);
    q.progress(4, 240_000_000, Some(700_000_000), 0.0);
    q.mark_running(5);
    q.max_attempts = 1;
    q.fail(5, "404 Not Found (no source served this object)".into());
    let mut ui = Ui::new();
    ui.log.push("start #3 model-weights.safetensors");
    ui.log
        .push("warning #2: content is zstd (archive) but the name says tar.zst (archive)");
    ui.log.push("fail #5: 404 Not Found");
    ui.selected = 2;
    print!("{}", ui.render(&q, 108, 26));
    println!();
}

/// Run the interactive manager.
///
/// Transfers are driven by the same engine the one-shot CLI uses, so the
/// interactive path cannot drift from the scripted one.
/// Run the manager, optionally forcing headless mode.
///
/// `force_headless` exists because the detached worker must never try to take a terminal:
/// it has none, and probing for one would make the decision depend on how it was spawned.
///
/// Returns how many items ended in failure, so a headless run can exit non-zero.
pub async fn run_with(
    queue_path: PathBuf,
    initial: Vec<String>,
    max_active: usize,
    force_headless: bool,
    template: crate::download::Job,
) -> io::Result<usize> {
    if force_headless {
        return run_headless(queue_path, initial, max_active, &template).await;
    }
    // Raw mode requires a terminal. Without this check the failure surfaces as
    // "Operation not permitted (os error 1)", which tells the user nothing about
    // what to do. Headless mode below runs the same queue without a screen, so a
    // script or a CI job is not locked out of the queue manager.
    use std::io::IsTerminal as _;
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return run_headless(queue_path, initial, max_active, &template).await;
    }
    run_interactive(queue_path, initial, max_active, &template).await
}

/// Load the queue (or start a fresh one) and enqueue the URLs given on the
/// command line. The shared entry step of both manager modes — headless and
/// interactive must agree on how a queue resumes and how a bare URL is named.
fn load_queue(
    queue_path: &std::path::Path,
    initial: Vec<String>,
    max_active: usize,
) -> io::Result<Queue> {
    let mut q = Queue::load(queue_path)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?
        .unwrap_or_else(|| Queue::new(max_active));
    q.max_active = max_active.max(1);
    for url in initial {
        let name = crate::url::Url::parse(&url)
            .map(|u| u.suggested_filename())
            .unwrap_or_else(|| "download".into());
        q.add(vec![url], PathBuf::from(name));
    }
    Ok(q)
}

/// Drive the queue to completion with no terminal, logging one line per event.
///
/// Same queue, same engine, no screen: this is what runs under `nohup`, in CI, or
/// anywhere stdin is not a terminal.
pub async fn run_headless(
    queue_path: PathBuf,
    initial: Vec<String>,
    max_active: usize,
    template: &crate::download::Job,
) -> io::Result<usize> {
    let _headless = crate::plugin_ui::Headless::enter();
    let mut q = load_queue(&queue_path, initial, max_active)?;
    eprintln!(
        "hydra: no terminal; running the queue headless ({} items)",
        q.items.len()
    );
    let mut running: std::collections::HashMap<
        u64,
        tokio::task::JoinHandle<crate::download::Outcome>,
    > = std::collections::HashMap::new();

    // Headless mode still consumes ticks: one log line per interval is how a script or
    // CI job sees that a transfer is alive rather than hung.
    let (tick_tx, mut tick_rx) = tokio::sync::mpsc::unbounded_channel::<crate::download::Tick>();
    let mut last_log = std::time::Instant::now();

    while !q.is_idle() || !running.is_empty() {
        for id in q.to_start() {
            let Some(item) = q.get(id).cloned() else {
                continue;
            };
            q.mark_running(id);
            // Persist immediately: the queue file is how a reattaching session sees that
            // this item is live and which process owns it. Saving only on completion made
            // a detached worker's progress invisible — the UI reloaded, saw `Queued` with
            // no owner, and treated a running transfer as a phantom.
            let _ = q.save(&queue_path);
            eprintln!("hydra: start #{id} {}", item.name());
            running.insert(
                id,
                tokio::spawn(crate::download::run(job_for(
                    &item,
                    Some(tick_tx.clone()),
                    template,
                    None,
                ))),
            );
        }
        let done: Vec<u64> = running
            .iter()
            .filter(|(_, h)| h.is_finished())
            .map(|(i, _)| *i)
            .collect();
        for id in done {
            if let Some(h) = running.remove(&id) {
                match h.await {
                    Ok(out) if out.ok => {
                        q.progress(id, out.size, Some(out.size), 0.0);
                        q.finish(id, out.sha256.clone(), out.category.clone());
                        eprintln!(
                            "hydra: done #{id} {} {}",
                            hya_core::fmt::bytes(out.size),
                            out.category.unwrap_or_default()
                        );
                        if let Some(c) = out.format_conflict {
                            eprintln!("hydra: warning #{id}: {c}");
                        }
                    }
                    Ok(out) => {
                        let why = out.note.unwrap_or_else(|| "failed".into());
                        let st = q.fail(id, why.clone());
                        eprintln!("hydra: {} #{id}: {why}", st.as_str());
                    }
                    Err(e) => {
                        let st = q.fail(id, format!("task error: {e}"));
                        eprintln!("hydra: {} #{id}: task error: {e}", st.as_str());
                    }
                }
                let _ = q.save(&queue_path);
            }
        }
        // Drain live progress: without this the queue only learned a transfer's size
        // when it FINISHED, so nothing moved for the whole download.
        while let Ok(tk) = tick_rx.try_recv() {
            q.progress(tk.id, tk.done, tk.size, tk.rate);
        }
        if last_log.elapsed().as_secs_f64() >= 2.0 && !q.is_idle() {
            last_log = std::time::Instant::now();
            // Checkpoint progress so a reattaching UI shows real numbers rather than the
            // values from whenever the last item finished.
            let _ = q.save(&queue_path);
            for it in q
                .items
                .iter()
                .filter(|i| i.state == crate::queue::State::Running)
            {
                eprintln!(
                    "hydra: #{} {} {} {}",
                    it.id,
                    it.name(),
                    match (it.done_bytes, it.size) {
                        (d, Some(s)) if s > 0 => format!("{:.1}%", 100.0 * d as f64 / s as f64),
                        (d, _) => hya_core::fmt::bytes(d),
                    },
                    hya_core::fmt::bytes(it.rate as u64) + "/s"
                );
            }
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let (_, _, done, failed) = q.counts();
    eprintln!("hydra: queue finished — {done} done, {failed} failed");
    let _ = q.save(&queue_path);
    Ok(failed)
}

/// Start a detached process that keeps working the queue after the UI exits.
///
/// A re-exec of this binary in headless queue mode, in its own session so it survives the
/// terminal closing, with its streams sent to a log file beside the queue so a later
/// session can see what happened rather than losing the output to /dev/null.
fn spawn_worker(queue_path: &std::path::Path, max_active: usize) -> io::Result<u32> {
    let exe = std::env::current_exe()?;
    let log_path = queue_path.with_extension("log");
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)?;
    let err = log.try_clone()?;
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("interactive")
        .arg("--headless")
        .arg("--queue-file")
        .arg(queue_path)
        .arg("--max-active")
        .arg(max_active.to_string())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::from(log))
        .stderr(std::process::Stdio::from(err));
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        // setsid: a new session with no controlling terminal, so closing the terminal
        // does not deliver SIGHUP to the worker.
        unsafe {
            cmd.pre_exec(|| {
                extern "C" {
                    fn setsid() -> i32;
                }
                if setsid() == -1 {
                    // Already a session leader is fine; anything else is not fatal
                    // either, so do not fail the detach over it.
                }
                Ok(())
            });
        }
    }
    let child = cmd.spawn()?;
    Ok(child.id())
}

/// The job the queue manager runs for one item, in either mode.
///
/// `template` carries the command line's download flags (headers, rate cap,
/// proxy, cookies, connection count); the engine must stay quiet and
/// progress-free because the manager owns the screen.
fn job_for(
    item: &crate::queue::Item,
    ticks: Option<tokio::sync::mpsc::UnboundedSender<crate::download::Tick>>,
    template: &crate::download::Job,
    cancel: Option<Arc<AtomicBool>>,
) -> crate::download::Job {
    crate::download::Job {
        ticks: ticks.map(|tx| (item.id, tx)),
        urls: item.urls.clone(),
        output: Some(item.output.clone()),
        resume: true,
        create_dirs: true,
        force: true, // queued items were already decided by the queue, not a prompt
        quiet: true,
        no_progress: true,
        to_stdout: false,
        no_save: false,
        spider: false,
        cancel: cancel.or_else(|| template.cancel.clone()),
        ..template.clone()
    }
}

async fn run_interactive(
    queue_path: PathBuf,
    initial: Vec<String>,
    max_active: usize,
    template: &crate::download::Job,
) -> io::Result<usize> {
    let mut q = load_queue(&queue_path, initial, max_active)?;

    let _guard = TerminalGuard::enter()?;
    let (_plugin_guard, plugin_events) = crate::plugin_ui::attach();
    let mut pending_prompts = std::collections::VecDeque::new();
    let mut ui = Ui::new();
    // Live progress from running transfers. Unbounded because dropping a tick is
    // harmless (the next one supersedes it) but blocking a transfer to deliver one is
    // not — a UI must never be able to stall the download it is displaying.
    let (tick_tx, mut tick_rx) = tokio::sync::mpsc::unbounded_channel::<crate::download::Tick>();
    // PID of the detached worker, when `b` handed the queue over.
    let mut background_pid: Option<u32> = None;
    ui.log.push("ready");

    // Running transfers, keyed by queue id, with the flag that stops each one.
    // Setting the flag is what lets the engine end its range tasks and write
    // the resume record; aborting the task alone left the sockets open until
    // the runtime got round to dropping them.
    let mut running: std::collections::HashMap<
        u64,
        tokio::task::JoinHandle<crate::download::Outcome>,
    > = std::collections::HashMap::new();
    let mut stops: std::collections::HashMap<u64, Arc<AtomicBool>> =
        std::collections::HashMap::new();

    loop {
        // ---- start whatever may start ----
        for id in q.to_start() {
            let Some(item) = q.get(id).cloned() else {
                continue;
            };
            q.mark_running(id);
            ui.log.push(format!("start #{id} {}", item.name()));
            let stop = Arc::new(AtomicBool::new(false));
            stops.insert(id, stop.clone());
            running.insert(
                id,
                tokio::spawn(crate::download::run(job_for(
                    &item,
                    Some(tick_tx.clone()),
                    template,
                    Some(stop),
                ))),
            );
        }

        // ---- reap finished transfers ----
        let finished: Vec<u64> = running
            .iter()
            .filter(|(_, h)| h.is_finished())
            .map(|(id, _)| *id)
            .collect();
        for id in finished {
            stops.remove(&id);
            if let Some(h) = running.remove(&id) {
                match h.await {
                    Ok(out) if out.ok => {
                        q.progress(id, out.size, Some(out.size), 0.0);
                        q.finish(id, out.sha256.clone(), out.category.clone());
                        ui.log.push(format!(
                            "done #{id} {} {}",
                            hya_core::fmt::bytes(out.size),
                            out.category.unwrap_or_default()
                        ));
                        if let Some(c) = out.format_conflict {
                            ui.log.push(format!("warning #{id}: {c}"));
                        }
                    }
                    Ok(out) => {
                        let why = out.note.unwrap_or_else(|| "failed".into());
                        let st = q.fail(id, why.clone());
                        ui.log.push(format!("{} #{id}: {why}", st.as_str()));
                    }
                    Err(e) => {
                        let st = q.fail(id, format!("task error: {e}"));
                        ui.log.push(format!("{} #{id}: task error", st.as_str()));
                    }
                }
            }
        }

        while let Ok(event) = plugin_events.try_recv() {
            match event {
                crate::plugin_ui::Event::Prompt(prompt) => pending_prompts.push_back(prompt),
                crate::plugin_ui::Event::Log(line) => ui.log.push(crate::plugin_ui::clean(&line)),
                crate::plugin_ui::Event::Installed(result) => match result {
                    Ok(plugins) => ui.plugins = plugins,
                    Err(e) => ui.log.push(e),
                },
            }
        }
        if ui.plugin_prompt.is_none() {
            if let Some(prompt) = pending_prompts.pop_front() {
                ui.plugin_prompt = Some(prompt);
                ui.mode = Mode::Form;
            }
        }
        // ---- input ----
        if event::poll(Duration::from_millis(120))? {
            if let Event::Key(k) = event::read()? {
                match ui.on_key(k, &q) {
                    Command::Quit => break,
                    Command::FormCancel | Command::FormSubmit => {
                        let submit = k.code == KeyCode::Enter;
                        if let Some(prompt) = &mut ui.plugin_prompt {
                            let result = if submit {
                                prompt.submit()
                            } else {
                                Err(hya_plugin_api::PluginError::new(
                                    hya_plugin_api::ErrorCode::Cancelled,
                                    "prompt cancelled",
                                ))
                            };
                            if submit && result.is_err() {
                                prompt.error = result.err().map(|e| e.to_string());
                            } else {
                                let _ = prompt.reply.send(result);
                                ui.plugin_prompt = None;
                                ui.mode = Mode::List;
                            }
                        }
                    }
                    Command::PluginInstall(path) => {
                        match hya_plugin::manager::Manager::inspect(std::path::Path::new(&path)) {
                            Ok(package) => {
                                ui.plugin_review = Some((path.into(), package.manifest));
                                ui.mode = Mode::PluginConsent;
                            }
                            Err(e) => ui.log.push(e.to_string()),
                        }
                    }
                    Command::PluginSettings(id) => {
                        if let Some(plugin) =
                            ui.plugins.iter().find(|p| p.manifest.id == id).cloned()
                        {
                            let (reply, receiver) = std::sync::mpsc::sync_channel(1);
                            let fields = plugin
                                .manifest
                                .settings
                                .iter()
                                .map(|field| {
                                    let mut field = field.clone();
                                    field.default =
                                        plugin.settings.get(&field.key).cloned().or(field.default);
                                    field
                                })
                                .collect();
                            ui.plugin_prompt = Some(crate::plugin_ui::Prompt {
                                plugin: id.clone(),
                                form: hya_plugin_api::Form {
                                    title: Some("Settings".into()),
                                    fields,
                                },
                                values: Default::default(),
                                selected: 0,
                                error: None,
                                reply,
                            });
                            ui.mode = Mode::Form;
                            tokio::task::spawn_blocking(move || {
                                let result = (|| -> Result<_, String> {
                                    let values = receiver
                                        .recv()
                                        .map_err(|e| e.to_string())?
                                        .map_err(|e| e.to_string())?;
                                    let mut manager =
                                        hya_plugin::manager::Manager::open_with_official(
                                            hya_plugin::hydra_dir().join("plugins"),
                                        )
                                        .map_err(|e| e.to_string())?;
                                    for (key, value) in values {
                                        if plugin.manifest.settings.iter().any(|f| {
                                            f.key == key
                                                && f.kind == hya_plugin_api::FieldKind::Secret
                                        }) {
                                            let secret = value.as_text().unwrap_or_default();
                                            if !secret.is_empty() {
                                                manager
                                                    .set_secret(&id, &key, secret.into())
                                                    .map_err(|e| e.to_string())?;
                                            }
                                        } else {
                                            manager
                                                .set(&id, &key, value)
                                                .map_err(|e| e.to_string())?;
                                        }
                                    }
                                    manager
                                        .check(
                                            &id,
                                            hya_net::tls::TlsCapableConnector::new()
                                                .map_err(|e| e.to_string())?,
                                            crate::plugins::frontend(&id),
                                        )
                                        .map_err(|e| e.to_string())?;
                                    Ok(manager.list().to_vec())
                                })();
                                crate::plugin_ui::send(crate::plugin_ui::Event::Installed(result));
                            });
                        }
                    }
                    operation @ (Command::PluginLoad
                    | Command::PluginToggle(..)
                    | Command::PluginMove(..)
                    | Command::PluginConfirm
                    | Command::PluginPermission(..)) => {
                        let review = ui.plugin_review.take();
                        ui.mode = Mode::Plugins;
                        tokio::task::spawn_blocking(move || {
                            let result = (|| -> Result<_, String> {
                                let mut manager = hya_plugin::manager::Manager::open_with_official(
                                    hya_plugin::hydra_dir().join("plugins"),
                                )
                                .map_err(|e| e.to_string())?;
                                match operation {
                                    Command::PluginPermission(id, capability) => {
                                        let (grant, capability) = capability
                                            .strip_prefix('-')
                                            .map(|s| (false, s))
                                            .unwrap_or((
                                                true,
                                                capability.strip_prefix('+').unwrap_or(&capability),
                                            ));
                                        manager
                                            .permission(&id, capability, grant)
                                            .map_err(|e| e.to_string())?;
                                    }
                                    Command::PluginToggle(id, enabled) => {
                                        manager.enable(&id, enabled).map_err(|e| e.to_string())?
                                    }
                                    Command::PluginMove(id, delta) => {
                                        let mut ids: Vec<_> = manager
                                            .list()
                                            .iter()
                                            .map(|p| p.manifest.id.clone())
                                            .collect();
                                        if let Some(index) = ids.iter().position(|p| p == &id) {
                                            let next = index
                                                .saturating_add_signed(delta)
                                                .min(ids.len() - 1);
                                            ids.swap(index, next);
                                            manager.order(&ids).map_err(|e| e.to_string())?;
                                        }
                                    }
                                    Command::PluginConfirm => {
                                        if let Some((path, manifest)) = review {
                                            if hya_plugin::manager::Manager::inspect(&path)
                                                .map_err(|e| e.to_string())?
                                                .manifest
                                                != manifest
                                            {
                                                return Err("package changed; review again".into());
                                            }
                                            manager
                                                .install(&path, manifest.permissions)
                                                .map_err(|e| e.to_string())?;
                                        }
                                    }
                                    _ => {}
                                }
                                Ok(manager.list().to_vec())
                            })();
                            crate::plugin_ui::send(crate::plugin_ui::Event::Installed(result));
                        });
                    }
                    Command::Pause(id) => {
                        q.pause(id);
                        if let Some(stop) = stops.remove(&id) {
                            stop.store(true, Ordering::Relaxed);
                        }
                        if let Some(h) = running.remove(&id) {
                            // The engine writes its resume record from what
                            // it held on the way out; the join is short.
                            let _ = h.await;
                        }
                        ui.log.push(format!("paused #{id}"));
                    }
                    Command::Resume(id) => {
                        q.resume(id);
                        ui.log.push(format!("resumed #{id}"));
                    }
                    Command::Cancel(id) => {
                        q.cancel(id);
                        if let Some(stop) = stops.remove(&id) {
                            stop.store(true, Ordering::Relaxed);
                        }
                        if let Some(h) = running.remove(&id) {
                            let _ = h.await;
                        }
                        ui.log.push(format!("cancelled #{id}"));
                    }
                    Command::MoveUp(id) => q.reorder(id, -1),
                    Command::MoveDown(id) => q.reorder(id, 1),
                    Command::ClearFinished => {
                        let n = q.clear_finished();
                        ui.log.push(format!("cleared {n}"));
                    }
                    Command::Concurrency(d) => {
                        q.max_active = (q.max_active as isize + d).clamp(1, 16) as usize;
                        ui.log.push(format!(
                            "max {} transfer(s) at once (each still opens its own connections)",
                            q.max_active
                        ));
                    }
                    // Open/close the detail screen. The queue is untouched — these are
                    // view changes, and keeping them out of the queue state is what
                    // makes both testable without a terminal.
                    Command::OpenDetail(id) => {
                        ui.detail = Some(id);
                        ui.mode = Mode::Detail;
                    }
                    Command::CloseDetail => {}
                    Command::Background => {
                        // Backgrounding cannot just leave the loop: the transfers are
                        // tasks in THIS process, so exiting kills them and a later session
                        // finds everything stopped. Hand the queue to a detached worker
                        // instead, which is what "keeps running" has to mean.
                        //
                        // In-process transfers are aborted first and their items demoted,
                        // because two processes writing one file is worse than restarting
                        // from a checkpoint — and the sidecar is checkpointed during the
                        // transfer, so the worker resumes rather than refetching.
                        for (id, h) in running.drain() {
                            h.abort();
                            if let Some(i) = q.items.iter_mut().find(|i| i.id == id) {
                                i.state = crate::queue::State::Queued;
                                i.owner_pid = None;
                                i.rate = 0.0;
                            }
                        }
                        let _ = q.save(&queue_path);
                        match spawn_worker(&queue_path, q.max_active) {
                            Ok(pid) => {
                                background_pid = Some(pid);
                                break;
                            }
                            Err(e) => {
                                ui.log.push(format!("could not detach: {e}"));
                            }
                        }
                    }
                    Command::Add(url) => match crate::url::Url::parse(&url) {
                        Some(u) => {
                            let id =
                                q.add(vec![url.clone()], PathBuf::from(u.suggested_filename()));
                            ui.log
                                .push(format!("queued #{id} {}", u.suggested_filename()));
                        }
                        None => ui.log.push(format!("not a usable URL: {url}")),
                    },
                    Command::None => {}
                }
                ui.clamp_selection(&q);
            }
        }

        ui.note_rate(q.total_rate());
        let (cols, rows) = terminal::size().unwrap_or((100, 30));
        while let Ok(tk) = tick_rx.try_recv() {
            q.progress(tk.id, tk.done, tk.size, tk.rate);
            ui.live.insert(tk.id, tk);
        }
        // The detail screen replaces the list rather than overlaying it: the
        // per-connection table needs the full width, and a split view at 80 columns
        // truncates both halves into uselessness.
        let frame = match (&ui.mode, ui.detail) {
            (Mode::Form, _) => format!(
                "\x1b[2J\x1b[H{}",
                ui.plugin_prompt
                    .as_ref()
                    .map(|p| p.render())
                    .unwrap_or_default()
            ),
            (
                Mode::Plugins | Mode::PluginPath(_) | Mode::PluginConsent | Mode::PluginGrant(..),
                _,
            ) => {
                let mut text = "\x1b[2J\x1b[HPlugins\r\n\r\n".to_string();
                for (index, p) in ui.plugins.iter().enumerate() {
                    text.push_str(&format!(
                        "{} [{}] {} | {} | {} | {}{}\r\n",
                        if index == ui.plugin_selected {
                            ">"
                        } else {
                            " "
                        },
                        if p.enabled { "x" } else { " " },
                        crate::plugin_ui::clean(&p.manifest.name),
                        crate::plugin_ui::clean(&p.manifest.id),
                        crate::plugin_ui::clean(p.manifest.author.as_deref().unwrap_or("—")),
                        crate::plugin_ui::clean(&p.manifest.version),
                        if p.dev { " (dev)" } else { "" }
                    ));
                }
                if let Mode::PluginGrant(id, capability) = &ui.mode {
                    if let Some(plugin) = ui.plugins.iter().find(|p| &p.manifest.id == id) {
                        text.push_str(&format!(
                            "\r\nDeclared: {}\r\n",
                            crate::plugin_ui::clean(
                                &hya_plugin::consent(&plugin.manifest.permissions).join(" | ")
                            )
                        ));
                    }
                    text.push_str(&format!("\r\nCapability (+ grant, - revoke): {}\r\nUse http:HOST, sources:HOST, cookies:HOST, exec:PROGRAM or data\r\nEnter apply · Esc cancel",crate::plugin_ui::clean(capability)));
                } else if let Mode::PluginPath(path) = &ui.mode {
                    text.push_str(&format!(
                        "\r\nInstall path: {}\r\nEnter review · Esc cancel",
                        crate::plugin_ui::clean(path)
                    ));
                } else if let Some((_, manifest)) = &ui.plugin_review {
                    text.push_str(&format!(
                        "\r\n{} ({})\r\n{}\r\ny accept and install · n cancel",
                        crate::plugin_ui::clean(&manifest.name),
                        crate::plugin_ui::clean(&manifest.id),
                        crate::plugin_ui::clean(
                            &hya_plugin::consent(&manifest.permissions).join(" | ")
                        )
                    ));
                } else {
                    text.push_str("\r\ne enable/disable · s settings · g grants · i install · J/K order · Esc back\r\n");
                }
                text
            }
            (Mode::Detail, Some(id)) => ui.render_detail(&q, id, cols, rows),
            _ => ui.render(&q, cols, rows),
        };
        let mut so = io::stdout();
        so.write_all(frame.as_bytes())?;
        so.flush()?;

        // Persist so a crash or a quit does not lose the plan.
        let _ = q.save(&queue_path);
    }

    // On `b` the queue was already handed to a detached worker, so the items must stay
    // queued for it to pick up — pausing them here would stop the very transfers the user
    // asked to keep running.
    if let Some(pid) = background_pid {
        let _ = q.save(&queue_path);
        eprintln!(
            "hydra: detached — worker pid {pid} is continuing {} item(s)",
            q.items.iter().filter(|i| !i.state.is_terminal()).count()
        );
        eprintln!("       queue:  {}", queue_path.display());
        eprintln!(
            "       log:    {}",
            queue_path.with_extension("log").display()
        );
        eprintln!(
            "       reattach with:  hydra interactive --queue-file {}",
            queue_path.display()
        );
        return Ok(0);
    }

    // Anything still running goes back in line, not on hold: its bytes and
    // sidecar are on disk and the next session resumes it before anything
    // that was waiting behind it. Pausing here read as a user's pause.
    for stop in stops.values() {
        stop.store(true, Ordering::Relaxed);
    }
    for (id, h) in running.drain() {
        let _ = h.await;
        q.interrupt(id);
    }
    let _ = q.save(&queue_path);
    let (_, _, _, failed) = q.counts();
    Ok(failed)
}

#[cfg(test)]
mod tests {
    /// `--headless` exited 0 with failed items in the queue; a script driving
    /// it had no way to know.
    #[tokio::test]
    async fn a_headless_run_reports_how_many_items_failed() {
        let dir = std::env::temp_dir().join(format!("hydra_headless_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let queue = dir.join("queue.json");
        // A port nobody listens on fails at once.
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        drop(l);
        let mut template = crate::download::default_job();
        template.no_proxy = true;
        template.output_dir = Some(dir.clone());
        let failed = super::run_headless(
            queue.clone(),
            vec![format!("http://127.0.0.1:{port}/gone.bin")],
            1,
            &template,
        )
        .await
        .unwrap();
        assert_eq!(failed, 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    use super::*;

    fn key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }
    fn shift(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::SHIFT)
    }

    fn q2() -> Queue {
        let mut q = Queue::new(2);
        q.add(vec!["http://a/one.iso".into()], "one.iso".into());
        q.add(vec!["http://a/two.zip".into()], "two.zip".into());
        q
    }

    #[test]
    fn plugin_navigation_settings_permissions_and_consent_bindings() {
        let mut ui = Ui::new();
        let q = q2();
        assert!(matches!(ui.on_key(key('P'), &q), Command::PluginLoad));
        assert_eq!(ui.mode, Mode::Plugins);
        assert!(matches!(ui.on_key(key('s'), &q), Command::None));
        let manifest: hya_plugin_api::Manifest = serde_json::from_value(serde_json::json!({"id":"example.direct","name":"Direct","version":"1.0.0","api":1,"module":"plugin.wasm"})).unwrap();
        ui.plugins.push(hya_plugin::manager::Installed {
            grants: manifest.permissions.clone(),
            manifest,
            directory: "unused".into(),
            dev: false,
            signing: Default::default(),
            enabled: true,
            pins: Default::default(),
            settings: Default::default(),
            failures: 0,
            module_sha256: String::new(),
            native_sha256: Default::default(),
            previous: None,
        });
        assert!(
            matches!(ui.on_key(key('s'),&q),Command::PluginSettings(id) if id == "example.direct")
        );
        assert!(matches!(
            ui.on_key(key('e'), &q),
            Command::PluginToggle(_, false)
        ));
        assert!(matches!(ui.on_key(key('J'), &q), Command::PluginMove(_, 1)));
        assert!(matches!(
            ui.on_key(key('K'), &q),
            Command::PluginMove(_, -1)
        ));
        ui.on_key(key('j'), &q);
        assert_eq!(ui.plugin_selected, 0);
        ui.on_key(key('k'), &q);
        ui.on_key(key('g'), &q);
        for c in "+datax".chars() {
            ui.on_key(key(c), &q);
        }
        ui.on_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE), &q);
        assert!(
            matches!(ui.on_key(KeyEvent::new(KeyCode::Enter,KeyModifiers::NONE),&q),Command::PluginPermission(_,cap) if cap == "+data")
        );
        ui.on_key(key('i'), &q);
        for c in "test.hyaplugin".chars() {
            ui.on_key(key(c), &q);
        }
        assert!(
            matches!(ui.on_key(KeyEvent::new(KeyCode::Enter,KeyModifiers::NONE),&q),Command::PluginInstall(path) if path == "test.hyaplugin")
        );
        ui.mode = Mode::PluginConsent;
        assert!(matches!(ui.on_key(key('y'), &q), Command::PluginConfirm));
        ui.on_key(key('n'), &q);
        assert_eq!(ui.mode, Mode::Plugins);
        ui.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &q);
        assert_eq!(ui.mode, Mode::List);
    }
    #[test]
    fn plugin_form_edits_fields_without_leaking_secret_and_maps_cancel() {
        let mut ui = Ui::new();
        let q = q2();
        let (reply, _) = std::sync::mpsc::sync_channel(1);
        let form=serde_json::from_value(serde_json::json!({"fields":[{"key":"secret","label":"Token","type":"secret"},{"key":"count","label":"Count","type":"number"}]})).unwrap();
        ui.plugin_prompt = Some(crate::plugin_ui::Prompt {
            plugin: "example.direct".into(),
            form,
            values: Default::default(),
            selected: 0,
            error: None,
            reply,
        });
        ui.mode = Mode::Form;
        for c in "privatex".chars() {
            ui.on_key(key(c), &q);
        }
        ui.on_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE), &q);
        ui.on_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE), &q);
        ui.on_key(key('2'), &q);
        ui.on_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE), &q);
        let prompt = ui.plugin_prompt.as_ref().unwrap();
        assert_eq!(prompt.selected, 0);
        assert!(!prompt.render().contains("private"));
        assert!(prompt.submit().is_ok());
        assert!(matches!(
            ui.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &q),
            Command::FormSubmit
        ));
        assert!(matches!(
            ui.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &q),
            Command::FormCancel
        ));
    }
    #[test]
    fn navigation_stays_in_bounds() {
        let mut ui = Ui::new();
        let q = q2();
        for _ in 0..5 {
            ui.on_key(key('j'), &q);
        }
        assert_eq!(ui.selected, 1, "must not run past the last item");
        for _ in 0..5 {
            ui.on_key(key('k'), &q);
        }
        assert_eq!(ui.selected, 0, "must not run before the first item");
    }

    #[test]
    fn navigation_on_an_empty_queue_does_not_panic() {
        let mut ui = Ui::new();
        let q = Queue::new(1);
        assert_eq!(ui.on_key(key('j'), &q), Command::None);
        assert_eq!(
            ui.on_key(key('p'), &q),
            Command::None,
            "nothing selected, nothing to pause"
        );
        assert_eq!(ui.selected, 0);
    }

    #[test]
    fn bindings_map_to_the_selected_item() {
        let mut ui = Ui::new();
        let q = q2();
        ui.on_key(key('j'), &q);
        assert_eq!(ui.on_key(key('p'), &q), Command::Pause(2));
        assert_eq!(ui.on_key(key('r'), &q), Command::Resume(2));
        assert_eq!(ui.on_key(key('d'), &q), Command::Cancel(2));
        assert_eq!(ui.on_key(shift('K'), &q), Command::MoveUp(2));
        assert_eq!(ui.on_key(shift('J'), &q), Command::MoveDown(2));
    }

    #[test]
    fn concurrency_and_clear_are_global_not_per_item() {
        let mut ui = Ui::new();
        let q = q2();
        assert_eq!(ui.on_key(key('+'), &q), Command::Concurrency(1));
        assert_eq!(ui.on_key(key('-'), &q), Command::Concurrency(-1));
        assert_eq!(ui.on_key(key('c'), &q), Command::ClearFinished);
    }

    #[test]
    fn adding_a_url_is_typed_and_confirmed() {
        let mut ui = Ui::new();
        let q = q2();
        assert_eq!(ui.on_key(key('a'), &q), Command::None);
        assert!(matches!(ui.mode, Mode::Adding(_)));
        for c in "http://x/f".chars() {
            ui.on_key(key(c), &q);
        }
        // Typed characters must not be interpreted as bindings: 'p' is in the URL.
        assert_eq!(
            ui.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &q),
            Command::Add("http://x/f".into())
        );
        assert_eq!(ui.mode, Mode::List);
    }

    #[test]
    fn backspace_and_escape_work_while_typing() {
        let mut ui = Ui::new();
        let q = q2();
        ui.on_key(key('a'), &q);
        for c in "htp".chars() {
            ui.on_key(key(c), &q);
        }
        ui.on_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE), &q);
        assert_eq!(ui.mode, Mode::Adding("ht".into()));
        ui.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &q);
        assert_eq!(ui.mode, Mode::List, "escape must abandon the input");
    }

    #[test]
    fn an_empty_url_is_not_added() {
        let mut ui = Ui::new();
        let q = q2();
        ui.on_key(key('a'), &q);
        assert_eq!(
            ui.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &q),
            Command::None
        );
    }

    #[test]
    fn ctrl_c_quits_even_mid_typing() {
        let mut ui = Ui::new();
        let q = q2();
        ui.on_key(key('a'), &q);
        ui.on_key(key('h'), &q);
        let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(
            ui.on_key(ctrl_c, &q),
            Command::Quit,
            "a user must always be able to get out"
        );
    }

    #[test]
    fn help_is_dismissed_by_any_key() {
        let mut ui = Ui::new();
        let q = q2();
        ui.on_key(key('?'), &q);
        assert_eq!(ui.mode, Mode::Help);
        ui.on_key(key('x'), &q);
        assert_eq!(ui.mode, Mode::List);
    }

    #[test]
    fn selection_is_clamped_after_items_disappear() {
        let mut ui = Ui::new();
        let mut q = q2();
        ui.on_key(key('j'), &q);
        assert_eq!(ui.selected, 1);
        q.items.clear();
        ui.clamp_selection(&q);
        assert_eq!(
            ui.selected, 0,
            "a stale index would index out of bounds when drawing"
        );
    }

    #[test]
    fn render_includes_every_item_and_its_state() {
        let mut q = q2();
        q.mark_running(1);
        q.progress(1, 5 << 20, Some(10 << 20), 3.5e6);
        q.mark_running(2);
        q.fail(2, "connection reset".into());
        let ui = Ui::new();
        let out = ui.render(&q, 100, 30);
        assert!(out.contains("one.iso"), "running item missing");
        assert!(out.contains("two.zip"), "failed item missing");
        assert!(out.contains("50.0%"), "progress not shown: {out}");
        assert!(
            out.contains("connection reset"),
            "the error must be visible, not hidden"
        );
        assert!(out.contains("run "), "state tag missing");
    }

    fn long_queue(n: usize) -> Queue {
        let mut q = Queue::new(1);
        for i in 0..n {
            q.add(
                vec![format!("http://h/file{i:02}.bin")],
                PathBuf::from(format!("file{i:02}.bin")),
            );
        }
        q
    }

    #[test]
    fn selection_past_the_visible_rows_scrolls_the_list() {
        let q = long_queue(30);
        let mut ui = Ui::new();
        let rows = 20u16;
        for _ in 0..29 {
            ui.on_key(key('j'), &q);
        }
        let out = ui.render(&q, 100, rows);
        assert!(
            out.contains("file29.bin"),
            "the selected last item must be drawn"
        );
        assert!(!out.contains("file00.bin"), "the window must have scrolled");
    }

    #[test]
    fn list_does_not_scroll_until_selection_leaves_the_window() {
        let q = long_queue(30);
        let mut ui = Ui::new();
        for _ in 0..5 {
            ui.on_key(key('j'), &q);
        }
        let out = ui.render(&q, 100, 20);
        assert!(out.contains("file00.bin"));
        assert!(out.contains("file05.bin"));
    }

    #[test]
    fn scrolling_back_up_brings_earlier_items_into_view() {
        let q = long_queue(30);
        let mut ui = Ui::new();
        ui.selected = 29;
        for _ in 0..29 {
            ui.on_key(key('k'), &q);
        }
        assert!(ui.render(&q, 100, 20).contains("file00.bin"));
    }

    #[test]
    fn error_lines_count_against_the_window() {
        let mut q = long_queue(10);
        for id in 1..=10 {
            q.mark_running(id);
            q.fail(id, "boom".into());
        }
        let mut ui = Ui::new();
        ui.selected = 9;
        let out = ui.render(&q, 100, 14);
        assert!(out.contains("file09.bin"));
        assert!(out.matches("boom").count() <= 3);
    }

    #[test]
    fn list_stays_within_the_screen_when_failed_items_take_two_lines() {
        let mut q = Queue::new(1);
        for n in 0..30 {
            let id = q.add(
                vec![format!("http://x/{n}.bin")],
                format!("f{n}.bin").into(),
            );
            q.mark_running(id);
            q.fail(id, "404 not found".into());
        }
        let ui = Ui::new();
        let out = ui.render(&q, 140, 24);
        assert!(
            out.matches("\r\n").count() < 24,
            "frame taller than the terminal scrolls the header off and leaves stale rows"
        );
    }

    #[test]
    fn selection_scrolls_into_view_past_the_last_visible_row() {
        let mut q = Queue::new(1);
        for n in 0..30 {
            q.add(
                vec![format!("http://x/{n}.bin")],
                format!("f{n}.bin").into(),
            );
        }
        let mut ui = Ui::new();
        ui.selected = 29;
        let out = ui.render(&q, 100, 24);
        assert!(out.contains("f29.bin"), "selected item must be drawn");
        assert!(!out.contains("f0.bin"), "window must have scrolled");
    }

    #[test]
    fn render_survives_a_tiny_terminal() {
        let q = q2();
        let ui = Ui::new();
        // A 1x1 terminal must not panic on a subtraction or a repeat count.
        for (c, r) in [(1u16, 1u16), (10, 3), (40, 8), (300, 100)] {
            let out = ui.render(&q, c, r);
            assert!(!out.is_empty());
        }
    }

    #[test]
    fn render_shows_the_empty_state_rather_than_a_blank_screen() {
        let q = Queue::new(2);
        let ui = Ui::new();
        let out = ui.render(&q, 80, 24);
        assert!(
            out.contains("empty"),
            "an empty queue must say so and say what to press"
        );
        assert!(out.contains("'a'"));
    }

    #[test]
    fn help_screen_explains_the_resume_caveat() {
        let mut ui = Ui::new();
        let q = q2();
        ui.on_key(key('?'), &q);
        let out = ui.render(&q, 100, 40);
        assert!(
            out.contains("validator"),
            "the help must state why resume can be refused, since that surprises people"
        );
    }
}
