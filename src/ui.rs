//! The curses-shaped TUI, on crossterm. Draws a centred box on whatever fd 1
//! points at, which main() has already aimed at /dev/tty.

use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::Arc;
use std::time::Duration;

use crossterm::cursor::{Hide, MoveTo, Show};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::style::{Attribute, Print, SetAttribute};
use crossterm::terminal::{self, Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::{execute, queue};
use serde_json::json;

use crate::api::{stream_completion, Msg};
use crate::config::Config;
use crate::ACCEPT_PREFIX;

/// Below this the box has no room for a border + prompt + one answer line.
const MIN_W: usize = 30;
const MIN_H: usize = 6;
const TICK: u64 = 50; // ms between repaints while streaming

#[derive(PartialEq, Clone, Copy)]
enum State {
    Ask,
    Stream,
    Browse,
}

/// Split the reply into pickable logical lines, dropping markdown fences.
pub fn selectable_lines(text: &str) -> Vec<String> {
    text.lines()
        .filter(|l| !l.trim_start().starts_with("```"))
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.trim_end().to_string())
        .collect()
}

/// Hard-wrap by character count (not bytes — answers can contain UTF-8).
pub fn wrap(line: &str, width: usize) -> Vec<String> {
    if width < 4 {
        return vec![line.to_string()];
    }
    let chars: Vec<char> = line.chars().collect();
    if chars.is_empty() {
        return vec![String::new()];
    }
    chars.chunks(width).map(|c| c.iter().collect()).collect()
}

fn clip(s: &str, width: usize) -> String {
    s.chars().take(width).collect()
}

fn pad_clip(s: &str, width: usize) -> String {
    let mut out = clip(s, width);
    let len = out.chars().count();
    if len < width {
        out.push_str(&" ".repeat(width - len));
    }
    out
}

/// Byte offset of the `i`th char; the query is edited by char index but
/// `String` mutators want bytes.
fn byte_at(s: &str, i: usize) -> usize {
    s.char_indices().nth(i).map(|(b, _)| b).unwrap_or(s.len())
}

pub struct Ui {
    cfg: Config,
    context: String,
    state: State,
    query: String,
    cursor: usize, // char index into query
    buffer: String,
    error: String,
    lines: Vec<String>,
    sel: usize,
    scroll: usize,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    stop: Arc<AtomicBool>,
    result: Option<String>,
}

impl Ui {
    pub fn new(cfg: Config, context: String) -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            cfg,
            context,
            state: State::Ask,
            query: String::new(),
            cursor: 0,
            buffer: String::new(),
            error: String::new(),
            lines: Vec::new(),
            sel: 0,
            scroll: 0,
            tx,
            rx,
            stop: Arc::new(AtomicBool::new(false)),
            result: None,
        }
    }

    // -- geometry ----------------------------------------------------------

    fn geom(&self, w: usize, h: usize) -> (usize, usize, usize, usize) {
        // Clamp to the screen LAST: a 40-column floor on a 38-column terminal
        // would otherwise put the box off the edge.
        let bw = (40.max(w * self.cfg.ui.width_pct as usize / 100)).min(MIN_W.max(w - 2));
        let bh = (8.max(h * self.cfg.ui.height_pct as usize / 100)).min(MIN_H.max(h - 2));
        ((h - bh) / 2, (w - bw) / 2, bh, bw)
    }

    // -- drawing -----------------------------------------------------------

    fn draw(&mut self, out: &mut impl Write) -> io::Result<()> {
        let (tw, th) = terminal::size()?;
        let (w, h) = (tw as usize, th as usize);
        queue!(out, Hide, Clear(ClearType::All))?;

        if w < MIN_W || h < MIN_H {
            queue!(
                out,
                MoveTo(0, 0),
                Print(clip("llmq: terminal too small", w.saturating_sub(1)))
            )?;
            return out.flush();
        }

        let (top, left, bh, bw) = self.geom(w, h);
        let (x, y) = (left as u16, top as u16);
        let inner_w = bw - 4;

        // border
        let bar: String = "─".repeat(bw - 2);
        queue!(out, MoveTo(x, y), Print(format!("┌{bar}┐")))?;
        for row in 1..bh - 1 {
            queue!(out, MoveTo(x, y + row as u16), Print("│"))?;
            queue!(out, MoveTo(x + bw as u16 - 1, y + row as u16), Print("│"))?;
        }
        queue!(
            out,
            MoveTo(x, y + bh as u16 - 1),
            Print(format!("└{bar}┘"))
        )?;

        let title = &self.cfg.ui.title;
        if title.chars().count() < bw - 4 {
            queue!(
                out,
                MoveTo(x + 2, y),
                SetAttribute(Attribute::Bold),
                Print(title),
                SetAttribute(Attribute::NormalIntensity)
            )?;
        }

        // prompt line
        let prompt = if self.state == State::Ask { "ask> " } else { "  q> " };
        let avail = inner_w.saturating_sub(prompt.len());
        let qc: Vec<char> = self.query.chars().collect();
        // Scroll so the cursor stays visible, not just the tail.
        let start = self.cursor.saturating_sub(avail);
        let shown: String = qc[start..(start + avail).min(qc.len())].iter().collect();
        queue!(
            out,
            MoveTo(x + 2, y + 1),
            SetAttribute(Attribute::Bold),
            Print(clip(&format!("{prompt}{shown}"), inner_w)),
            SetAttribute(Attribute::NormalIntensity)
        )?;
        queue!(out, MoveTo(x + 1, y + 2), Print(&bar))?;

        let body_top = y + 3;
        let body_h = bh - 5;

        if !self.error.is_empty() {
            for (i, row) in wrap(&self.error, inner_w).iter().take(body_h).enumerate() {
                queue!(
                    out,
                    MoveTo(x + 2, body_top + i as u16),
                    SetAttribute(Attribute::Bold),
                    Print(row),
                    SetAttribute(Attribute::NormalIntensity)
                )?;
            }
        } else if self.state == State::Stream {
            let mut rows: Vec<String> = Vec::new();
            let src: Vec<&str> = if self.buffer.is_empty() {
                vec![""]
            } else {
                self.buffer.lines().collect()
            };
            for line in src {
                rows.extend(wrap(line, inner_w));
            }
            let start = rows.len().saturating_sub(body_h);
            for (i, row) in rows[start..].iter().enumerate() {
                queue!(out, MoveTo(x + 2, body_top + i as u16), Print(clip(row, inner_w)))?;
            }
        } else if self.state == State::Browse {
            // (logical index, wrapped piece)
            let mut rows: Vec<(usize, String)> = Vec::new();
            for (idx, line) in self.lines.iter().enumerate() {
                for piece in wrap(line, inner_w) {
                    rows.push((idx, piece));
                }
            }
            let first = rows.iter().position(|r| r.0 == self.sel).unwrap_or(0);
            let last = rows.iter().rposition(|r| r.0 == self.sel).unwrap_or(0);
            if first < self.scroll {
                self.scroll = first;
            }
            if last >= self.scroll + body_h {
                self.scroll = last + 1 - body_h;
            }
            for (i, (idx, piece)) in rows.iter().skip(self.scroll).take(body_h).enumerate() {
                let selected = *idx == self.sel;
                if selected {
                    queue!(out, SetAttribute(Attribute::Reverse))?;
                }
                queue!(
                    out,
                    MoveTo(x + 2, body_top + i as u16),
                    Print(pad_clip(piece, inner_w))
                )?;
                if selected {
                    queue!(out, SetAttribute(Attribute::NoReverse))?;
                }
            }
        }

        queue!(out, MoveTo(x + 1, y + bh as u16 - 2), Print(&bar))?;
        queue!(
            out,
            MoveTo(x + 2, y + bh as u16 - 2),
            Print(clip(self.hint(), inner_w))
        )?;

        if self.state == State::Ask {
            let col = (2 + prompt.len() + (self.cursor - start)).min(bw - 2);
            queue!(out, MoveTo(x + col as u16, y + 1), Show)?;
        }
        out.flush()
    }

    fn hint(&self) -> &'static str {
        match self.state {
            State::Ask => " enter send · esc cancel ",
            State::Stream => " streaming… · esc abort ",
            State::Browse => " enter insert · ^a insert all · ^x insert+run · esc cancel ",
        }
    }

    // -- input -------------------------------------------------------------

    fn submit(&mut self) {
        self.state = State::Stream;
        self.buffer.clear();
        self.error.clear();
        self.stop.store(false, Ordering::Relaxed);

        let mut user = self.query.clone();
        if self.cfg.prompt.include_context && !self.context.trim().is_empty() {
            user.push_str(&format!(
                "\n\n[current command line: {}]",
                self.context.trim()
            ));
        }
        let messages = vec![
            json!({"role": "system", "content": self.cfg.prompt.system}),
            json!({"role": "user", "content": user}),
        ];

        let (cfg, tx, stop) = (self.cfg.clone(), self.tx.clone(), self.stop.clone());
        std::thread::spawn(move || stream_completion(&cfg, messages, tx, stop));
    }

    fn pump(&mut self) {
        loop {
            match self.rx.try_recv() {
                Ok(Msg::Chunk(c)) => self.buffer.push_str(&c),
                Ok(Msg::Error(e)) => self.error = e,
                Ok(Msg::Done) => {
                    self.lines = selectable_lines(&self.buffer);
                    self.sel = 0;
                    self.scroll = 0;
                    self.state = if self.lines.is_empty() {
                        State::Ask
                    } else {
                        State::Browse
                    };
                }
                Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => return,
            }
        }
    }

    /// Returns false to exit the loop.
    fn handle(&mut self, k: KeyEvent) -> bool {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);

        if k.code == KeyCode::Esc {
            if self.state == State::Stream {
                self.stop.store(true, Ordering::Relaxed);
                self.state = State::Ask;
                return true;
            }
            return false;
        }

        match self.state {
            State::Ask => {
                let len = self.query.chars().count();
                match k.code {
                    KeyCode::Enter => {
                        if !self.query.trim().is_empty() {
                            self.submit();
                        }
                    }
                    KeyCode::Left => self.cursor = self.cursor.saturating_sub(1),
                    KeyCode::Right => self.cursor = (self.cursor + 1).min(len),
                    KeyCode::Home => self.cursor = 0,
                    KeyCode::End => self.cursor = len,
                    KeyCode::Char('a') if ctrl => self.cursor = 0,
                    KeyCode::Char('e') if ctrl => self.cursor = len,
                    KeyCode::Char('b') if ctrl => self.cursor = self.cursor.saturating_sub(1),
                    KeyCode::Char('f') if ctrl => self.cursor = (self.cursor + 1).min(len),
                    KeyCode::Backspace => {
                        if self.cursor > 0 {
                            self.cursor -= 1;
                            let b = byte_at(&self.query, self.cursor);
                            self.query.remove(b);
                        }
                    }
                    KeyCode::Delete => {
                        if self.cursor < len {
                            let b = byte_at(&self.query, self.cursor);
                            self.query.remove(b);
                        }
                    }
                    KeyCode::Char('u') if ctrl => {
                        let b = byte_at(&self.query, self.cursor);
                        self.query = self.query.split_off(b);
                        self.cursor = 0;
                    }
                    KeyCode::Char('k') if ctrl => {
                        let b = byte_at(&self.query, self.cursor);
                        self.query.truncate(b);
                    }
                    KeyCode::Char('w') if ctrl => {
                        let mut chars: Vec<char> = self.query.chars().collect();
                        let mut i = self.cursor;
                        while i > 0 && chars[i - 1] == ' ' {
                            i -= 1;
                        }
                        while i > 0 && chars[i - 1] != ' ' {
                            i -= 1;
                        }
                        chars.drain(i..self.cursor);
                        self.query = chars.into_iter().collect();
                        self.cursor = i;
                    }
                    KeyCode::Char(c) if !ctrl && !c.is_control() => {
                        let b = byte_at(&self.query, self.cursor);
                        self.query.insert(b, c);
                        self.cursor += 1;
                    }
                    _ => {}
                }
                true
            }
            State::Stream => true,
            State::Browse => {
                match k.code {
                    KeyCode::Down => self.sel = (self.sel + 1).min(self.lines.len() - 1),
                    KeyCode::Up => self.sel = self.sel.saturating_sub(1),
                    KeyCode::Char('n') if ctrl => {
                        self.sel = (self.sel + 1).min(self.lines.len() - 1)
                    }
                    KeyCode::Char('p') if ctrl => self.sel = self.sel.saturating_sub(1),
                    KeyCode::Char('j') if !ctrl => {
                        self.sel = (self.sel + 1).min(self.lines.len() - 1)
                    }
                    KeyCode::Char('k') if !ctrl => self.sel = self.sel.saturating_sub(1),
                    KeyCode::Enter => {
                        self.result = Some(self.lines[self.sel].clone());
                        return false;
                    }
                    KeyCode::Char('a') if ctrl => {
                        self.result = Some(self.lines.join("\n"));
                        return false;
                    }
                    KeyCode::Char('x') if ctrl => {
                        self.result = Some(format!("{ACCEPT_PREFIX}{}", self.lines[self.sel]));
                        return false;
                    }
                    KeyCode::Char('e') | KeyCode::Char('/') if !ctrl => {
                        self.state = State::Ask;
                        self.cursor = self.query.chars().count();
                    }
                    _ => {}
                }
                true
            }
        }
    }

    // -- loop --------------------------------------------------------------

    pub fn run(&mut self) -> io::Result<Option<String>> {
        let mut out = io::stdout();
        terminal::enable_raw_mode()?;
        execute!(out, EnterAlternateScreen)?;

        let outcome = self.event_loop(&mut out);

        execute!(out, Show, LeaveAlternateScreen)?;
        terminal::disable_raw_mode()?;
        outcome
    }

    fn event_loop(&mut self, out: &mut impl Write) -> io::Result<Option<String>> {
        loop {
            self.pump();
            self.draw(out)?;
            if event::poll(Duration::from_millis(TICK))? {
                if let Event::Key(k) = event::read()? {
                    if k.kind == KeyEventKind::Press && !self.handle(k) {
                        return Ok(self.result.take());
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fences_and_blank_lines_are_dropped() {
        let text = "```bash\nls -la  \n\n```\nfd -t f\n";
        assert_eq!(selectable_lines(text), vec!["ls -la", "fd -t f"]);
    }

    #[test]
    fn wrap_splits_on_chars_not_bytes() {
        // "é" is two bytes; a byte-wise split would not give 4 chars per row.
        assert_eq!(wrap("aébcdéf", 4), vec!["aébc", "déf"]);
        assert_eq!(wrap("", 10), vec![""]);
        assert_eq!(wrap("abcdefghijkl", 4), vec!["abcd", "efgh", "ijkl"]);
    }

    #[test]
    fn wrap_gives_up_below_four_columns() {
        assert_eq!(wrap("hello", 3), vec!["hello"]);
    }

    #[test]
    fn clip_and_pad_count_chars() {
        assert_eq!(clip("héllo", 3), "hél");
        assert_eq!(pad_clip("hé", 4).chars().count(), 4);
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    fn ui_with(query: &str) -> Ui {
        let mut ui = Ui::new(Config::default(), String::new());
        for c in query.chars() {
            ui.handle(key(KeyCode::Char(c)));
        }
        ui
    }

    #[test]
    fn cursor_edits_mid_string_by_char_not_byte() {
        let mut ui = ui_with("héllo");
        ui.handle(key(KeyCode::Left));
        ui.handle(key(KeyCode::Left));
        ui.handle(key(KeyCode::Char('X')));
        assert_eq!(ui.query, "hélXlo");
        ui.handle(key(KeyCode::Home));
        ui.handle(key(KeyCode::Delete));
        assert_eq!(ui.query, "élXlo");
        ui.handle(key(KeyCode::End));
        ui.handle(key(KeyCode::Backspace));
        assert_eq!(ui.query, "élXl");
    }

    #[test]
    fn kill_bindings_are_cursor_aware() {
        let mut ui = ui_with("one two three");
        for _ in 0..6 {
            ui.handle(key(KeyCode::Left));
        }
        ui.handle(ctrl('w')); // deletes "two", not "three"
        assert_eq!(ui.query, "one  three");
        ui.handle(ctrl('k'));
        assert_eq!(ui.query, "one ");
        ui.handle(ctrl('u'));
        assert_eq!(ui.query, "");
    }

    #[test]
    fn cursor_never_leaves_the_query() {
        let mut ui = ui_with("ab");
        ui.handle(key(KeyCode::Right));
        assert_eq!(ui.cursor, 2);
        ui.handle(key(KeyCode::Home));
        ui.handle(key(KeyCode::Left));
        assert_eq!(ui.cursor, 0);
        ui.handle(key(KeyCode::Backspace));
        assert_eq!(ui.query, "ab");
    }
}
