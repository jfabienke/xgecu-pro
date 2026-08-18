// SPDX-FileCopyrightText: 2026 John Fabienke
// SPDX-License-Identifier: MIT

//! `minipro tui` — the ratatui front-end.
//!
//! Panels (per `docs/rust-redesign.md` §"3. TUI"): programmer status, a
//! searchable chip-DB browser, a live 40-pin ZIF contact map, an operation
//! progress gauge, a hex view, and a log pane. The render loop owns the
//! terminal; operations run elsewhere and stream [`UiMsg`]s over a channel
//! (see [`crate::reporters::TuiReporter`]) — the same `Event`/`Outcome` stream
//! the human and JSON reporters consume.

use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::Duration;

use minipro_core::device::Region;
use minipro_core::error::Result;
use minipro_core::ops;
use minipro_core::programmer::Txn;
use minipro_core::report::{Event, Outcome, Reporter};
use minipro_db::{ChipDb, XmlDb};
use ratatui::crossterm::event::{self, Event as TermEvent, KeyCode, KeyEventKind};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Gauge, List, ListItem, ListState, Paragraph};
use ratatui::Frame;

use crate::reporters::{link_label, TuiReporter, UiMsg};

/// How many chip-DB hits the browser fetches per search.
const BROWSER_LIMIT: usize = 200;
/// Log pane retention.
const LOG_CAP: usize = 200;

/// Per-pin state for the ZIF contact map.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PinState {
    /// No contact check has run yet.
    Unknown,
    Good,
    Bad,
}

/// All render state. Pure data + [`App::apply`]/[`App::set_search`] mutators,
/// so the update logic is unit-testable without a terminal.
pub struct App {
    // Chip-DB browser.
    db: Option<Box<dyn ChipDb + Send>>,
    search: String,
    hits: Vec<String>,
    hit_total: usize,
    hit_truncated: bool,
    list_state: ListState,
    // Programmer.
    programmer: Option<Box<dyn minipro_core::Programmer>>,
    status: String,
    // Live op state, fed by `UiMsg`s.
    pins: [PinState; 40],
    progress: Option<(u64, u64)>,
    hex: Vec<u8>,
    log: Vec<String>,
    // Event stream plumbing.
    rx: Receiver<UiMsg>,
    tx: Sender<UiMsg>,
    quit: bool,
}

impl App {
    pub fn new(db_dir: Option<PathBuf>) -> Self {
        let (tx, rx) = channel();
        let mut app = App {
            db: None,
            search: String::new(),
            hits: Vec::new(),
            hit_total: 0,
            hit_truncated: false,
            list_state: ListState::default(),
            programmer: None,
            status: "no programmer connected — press [c] to connect".into(),
            pins: [PinState::Unknown; 40],
            progress: None,
            hex: Vec::new(),
            log: Vec::new(),
            rx,
            tx,
            quit: false,
        };
        match db_dir {
            Some(dir) => match XmlDb::load(&dir) {
                Ok(db) => {
                    app.db = Some(Box::new(db));
                    app.refresh_hits();
                }
                Err(e) => app.push_log(format!("chip DB load failed: {e}")),
            },
            None => {
                app.push_log("no chip database (pass --db <dir> or set MINIPRO_DB_DIR)".to_string())
            }
        }
        app
    }

    /// A reporter whose events land in this app's channel — hand this to a
    /// worker thread running `minipro_core::ops` so progress, warnings, and
    /// the outcome render live.
    pub fn reporter(&self) -> TuiReporter {
        TuiReporter::from_sender(self.tx.clone())
    }

    fn push_log(&mut self, line: String) {
        self.log.push(line);
        if self.log.len() > LOG_CAP {
            self.log.remove(0);
        }
    }

    /// Apply one event-stream message to the render state.
    pub fn apply(&mut self, msg: UiMsg) {
        match msg {
            UiMsg::Progress { done, total } => self.progress = Some((done, total)),
            UiMsg::Note(n) => self.push_log(n),
            UiMsg::Warn(w) => self.push_log(format!("warning: {w}")),
            UiMsg::BadContact(open) => {
                for (i, pin) in self.pins.iter_mut().enumerate() {
                    let number = (i + 1) as u8;
                    *pin = if open.contains(&number) {
                        PinState::Bad
                    } else {
                        PinState::Good
                    };
                }
                self.push_log(format!("bad contact on pins {open:?}"));
            }
            UiMsg::Outcome(summary) => {
                self.push_log(summary);
                self.progress = None;
            }
            UiMsg::Hex(bytes) => self.hex = bytes,
            UiMsg::OpDone => {
                // The worker owned (and dropped) the programmer; make the
                // status honest and invite a reconnect.
                self.status = "no programmer connected — press [c] to connect".into();
                self.push_log("operation finished — press [c] to reconnect".into());
            }
        }
    }

    /// Update the search text and re-query the chip DB (capped + counted).
    pub fn set_search(&mut self, search: String) {
        self.search = search;
        self.refresh_hits();
    }

    fn refresh_hits(&mut self) {
        self.hits.clear();
        self.hit_total = 0;
        self.hit_truncated = false;
        if let Some(db) = &self.db {
            let found = db.search(&self.search, BROWSER_LIMIT);
            self.hit_total = found.total;
            self.hit_truncated = found.truncated;
            self.hits = found.hits.iter().map(|d| d.name.clone()).collect();
        }
        let select = if self.hits.is_empty() { None } else { Some(0) };
        self.list_state.select(select);
    }

    fn move_selection(&mut self, delta: i64) {
        if self.hits.is_empty() {
            return;
        }
        let current = self.list_state.selected().unwrap_or(0) as i64;
        let next = (current + delta).clamp(0, self.hits.len() as i64 - 1);
        self.list_state.select(Some(next as usize));
    }

    fn connect(&mut self) {
        match crate::open_programmer() {
            Ok(prog) => {
                let info = prog.info();
                self.status = format!(
                    "{} · fw {} · {} · {:.1} V · s/n {}",
                    info.model,
                    info.firmware,
                    link_label(info.link),
                    info.voltage,
                    info.serial
                );
                self.programmer = Some(prog);
                // Route a note through the *same* Reporter/Event path an op
                // would use, proving the stream wiring end to end.
                let mut rep = self.reporter();
                rep.event(&Event::Note("programmer connected".into()));
                rep.finish(&Outcome::Ok { op: "connect" });
            }
            Err(e) => {
                let mut line = format!("connect failed: {e}");
                if let Some(hint) = e.hint() {
                    line.push_str(&format!(" (hint: {hint})"));
                }
                self.push_log(line);
            }
        }
    }

    /// Resolve the selected chip and load its FPGA algorithm/bitstream — the
    /// same lookup + `ops::preflight` gate the CLI performs. Returns the
    /// ready-to-`begin` device plus the DB's firmware target for the mismatch
    /// advisory. Borrows `self` immutably only, so the caller can then take the
    /// programmer mutably. On any miss, returns a user-facing message.
    fn resolve_selected_device(
        &self,
    ) -> std::result::Result<(minipro_core::device::Device, minipro_core::error::FwVersion), String>
    {
        let db = self.db.as_ref().ok_or("no chip database loaded")?;
        let selected = self.list_state.selected().ok_or("select a chip first")?;
        let name = self.hits.get(selected).ok_or("selection out of range")?;
        let mut dev = db
            .get(name)
            .cloned()
            .ok_or("selected chip vanished from the database")?;
        // The shared capability gates: without this the TUI would happily
        // energize the socket for a part the CLI refuses (@ISP_VGA today).
        minipro_core::ops::preflight(&dev, minipro_core::ops::OpKind::Read)
            .map_err(|e| e.to_string())?;
        // Without this the FPGA drivers' `begin()` fails with "no bitstream
        // loaded" — the exact gap the TUI had.
        dev.algorithm = db
            .load_algorithm(&dev)
            .map_err(|e| format!("algorithm load failed: {e}"))?;
        Ok((dev, db.firmware_target()))
    }

    /// Read the selected chip on a worker thread, streaming progress /
    /// warnings / the outcome through the same `Reporter` pipeline the CLI
    /// commands use, plus the dump bytes for the hex view.
    ///
    /// TODO(hw): validated only against the trait surface — needs a real T76
    /// (sibling crates' `open`/`begin`/`read_block`) to exercise end to end.
    fn start_read(&mut self) {
        let (dev, fw_target) = match self.resolve_selected_device() {
            Ok(v) => v,
            Err(msg) => {
                self.push_log(msg);
                return;
            }
        };
        let Some(mut prog) = self.programmer.take() else {
            self.push_log("no programmer — press [c] to connect".into());
            return;
        };
        // Firmware-mismatch advisory, matching the CLI (main.rs `warn_firmware`).
        if prog.info().firmware != fw_target {
            self.push_log(
                "warning: programmer firmware differs from the database bitstream target".into(),
            );
        }
        // The worker owns the programmer for the duration of the op (both
        // `Programmer` and the reporter are `Send`, per the core's design).
        self.status.push_str(" · busy (reading)");
        let mut rep = self.reporter();
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = (|| {
                let link = prog.info().link;
                let verified = {
                    let mut txn = Txn::begin(&mut *prog, &dev)?;
                    let (p, s) = txn.parts();
                    ops::read_verified(p, s, Region::code(&dev), &mut rep)?
                };
                let _ = tx.send(UiMsg::Hex(verified.image.bytes.clone()));
                rep.finish(&verified.outcome(&dev.name, link));
                Ok::<(), minipro_core::Error>(())
            })();
            if let Err(e) = result {
                let _ = tx.send(UiMsg::Warn(format!("read failed: {e}")));
            }
            // Hand the programmer back to the render loop when the op ends.
            let _ = tx.send(UiMsg::OpDone);
        });
    }

    fn on_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Esc => self.quit = true,
            KeyCode::Char('q') if self.search.is_empty() => self.quit = true,
            KeyCode::Char('c') if self.search.is_empty() => self.connect(),
            KeyCode::Up => self.move_selection(-1),
            KeyCode::Down => self.move_selection(1),
            KeyCode::Enter => self.start_read(),
            KeyCode::Backspace => {
                let mut s = self.search.clone();
                s.pop();
                self.set_search(s);
            }
            KeyCode::Char(ch) => {
                let mut s = self.search.clone();
                s.push(ch);
                self.set_search(s);
            }
            _ => {}
        }
    }

    /// Drain any pending event-stream messages into render state.
    fn drain_events(&mut self) {
        while let Ok(msg) = self.rx.try_recv() {
            self.apply(msg);
        }
    }

    // -- rendering ----------------------------------------------------------

    fn draw(&mut self, frame: &mut Frame) {
        let [status_area, main_area, log_area, help_area] = Layout::vertical([
            Constraint::Length(3),
            Constraint::Min(12),
            Constraint::Length(6),
            Constraint::Length(1),
        ])
        .areas(frame.area());
        let [browser_area, right_area] =
            Layout::horizontal([Constraint::Percentage(40), Constraint::Percentage(60)])
                .areas(main_area);
        let [zif_area, gauge_area, hex_area] = Layout::vertical([
            Constraint::Length(6),
            Constraint::Length(3),
            Constraint::Min(4),
        ])
        .areas(right_area);

        self.draw_status(frame, status_area);
        self.draw_browser(frame, browser_area);
        self.draw_zif(frame, zif_area);
        self.draw_gauge(frame, gauge_area);
        self.draw_hex(frame, hex_area);
        self.draw_log(frame, log_area);
        frame.render_widget(
            Paragraph::new(
                " [q]/[Esc] quit · [c] connect · [↑↓] select · [Enter] read · type to search",
            )
            .style(Style::default().fg(Color::DarkGray)),
            help_area,
        );
    }

    fn draw_status(&self, frame: &mut Frame, area: Rect) {
        let style = if self.programmer.is_some() {
            Style::default().fg(Color::Green)
        } else {
            Style::default().fg(Color::Yellow)
        };
        frame.render_widget(
            Paragraph::new(self.status.as_str())
                .style(style)
                .block(Block::default().borders(Borders::ALL).title(" programmer ")),
            area,
        );
    }

    fn draw_browser(&mut self, frame: &mut Frame, area: Rect) {
        let title = if self.db.is_some() {
            let mut t = format!(" chips · /{} · {} match(es)", self.search, self.hit_total);
            if self.hit_truncated {
                t.push('+');
            }
            t.push(' ');
            t
        } else {
            " chips · no database loaded ".to_string()
        };
        let items: Vec<ListItem> = self
            .hits
            .iter()
            .map(|n| ListItem::new(n.as_str()))
            .collect();
        let list = List::new(items)
            .block(Block::default().borders(Borders::ALL).title(title))
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
            .highlight_symbol("▶ ");
        frame.render_stateful_widget(list, area, &mut self.list_state);
    }

    fn draw_zif(&self, frame: &mut Frame, area: Rect) {
        frame.render_widget(
            Paragraph::new(zif_lines(&self.pins)).block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" ZIF-40 contact "),
            ),
            area,
        );
    }

    fn draw_gauge(&self, frame: &mut Frame, area: Rect) {
        let (done, total) = self.progress.unwrap_or((0, 0));
        let ratio = if total == 0 {
            0.0
        } else {
            (done as f64 / total as f64).clamp(0.0, 1.0)
        };
        let label = if total == 0 {
            "idle".to_string()
        } else {
            format!("{done}/{total} bytes")
        };
        frame.render_widget(
            Gauge::default()
                .block(Block::default().borders(Borders::ALL).title(" progress "))
                .gauge_style(Style::default().fg(Color::Cyan))
                .ratio(ratio)
                .label(label),
            area,
        );
    }

    fn draw_hex(&self, frame: &mut Frame, area: Rect) {
        let rows = area.rows().count().saturating_sub(2); // borders
        let text: Vec<Line> = if self.hex.is_empty() {
            vec![Line::from(Span::styled(
                "no dump loaded",
                Style::default().fg(Color::DarkGray),
            ))]
        } else {
            hex_rows(&self.hex, rows)
                .into_iter()
                .map(Line::from)
                .collect()
        };
        frame.render_widget(
            Paragraph::new(text).block(Block::default().borders(Borders::ALL).title(" hex ")),
            area,
        );
    }

    fn draw_log(&self, frame: &mut Frame, area: Rect) {
        let visible = area.rows().count().saturating_sub(2);
        let start = self.log.len().saturating_sub(visible);
        let lines: Vec<Line> = self.log[start..]
            .iter()
            .map(|l| Line::from(l.as_str()))
            .collect();
        frame.render_widget(
            Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(" log ")),
            area,
        );
    }
}

/// The two socket-column lines of the ZIF map: pins 1–20 down the left, 40–21
/// up the right — rendered as `top = 1..=20`, `bottom = 40..=21` so opposing
/// pins align vertically, matching the physical socket.
fn zif_lines(pins: &[PinState; 40]) -> Vec<Line<'static>> {
    fn pin_span(number: u8, state: PinState) -> Span<'static> {
        let style = match state {
            PinState::Unknown => Style::default().fg(Color::DarkGray),
            PinState::Good => Style::default().fg(Color::Green),
            PinState::Bad => Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        };
        Span::styled(format!("{number:>3}"), style)
    }
    let top: Vec<Span> = (1..=20)
        .map(|n| pin_span(n, pins[n as usize - 1]))
        .collect();
    let bottom: Vec<Span> = (21..=40)
        .rev()
        .map(|n| pin_span(n, pins[n as usize - 1]))
        .collect();
    let legend = Line::from(vec![
        Span::styled("  ● good  ", Style::default().fg(Color::Green)),
        Span::styled("● bad  ", Style::default().fg(Color::Red)),
        Span::styled("● unknown", Style::default().fg(Color::DarkGray)),
    ]);
    vec![Line::from(top), Line::from(bottom), legend]
}

/// Classic hexdump rows (offset · 16 hex bytes · ASCII), capped at `max_rows`.
fn hex_rows(bytes: &[u8], max_rows: usize) -> Vec<String> {
    bytes
        .chunks(16)
        .take(max_rows)
        .enumerate()
        .map(|(i, chunk)| {
            let hex: Vec<String> = chunk.iter().map(|b| format!("{b:02x}")).collect();
            let ascii: String = chunk
                .iter()
                .map(|&b| {
                    if (0x20..0x7f).contains(&b) {
                        b as char
                    } else {
                        '.'
                    }
                })
                .collect();
            format!("{:06x}  {:<47}  |{}|", i * 16, hex.join(" "), ascii)
        })
        .collect()
}

/// Run the TUI until the user quits. Owns the terminal; `ratatui::init`
/// installs a panic hook that restores it.
pub fn run(db_dir: Option<PathBuf>) -> Result<()> {
    let mut terminal = ratatui::init();
    let mut app = App::new(db_dir);
    let result = loop {
        app.drain_events();
        if let Err(e) = terminal.draw(|frame| app.draw(frame)) {
            break Err(e.into());
        }
        match event::poll(Duration::from_millis(100)) {
            Ok(true) => match event::read() {
                Ok(TermEvent::Key(key)) if key.kind == KeyEventKind::Press => {
                    app.on_key(key.code);
                }
                Ok(_) => {}
                Err(e) => break Err(e.into()),
            },
            Ok(false) => {}
            Err(e) => break Err(e.into()),
        }
        if app.quit {
            break Ok(());
        }
    };
    ratatui::restore();
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_progress_and_outcome_updates_state() {
        let mut app = App::new(None);
        app.apply(UiMsg::Progress { done: 4, total: 8 });
        assert_eq!(app.progress, Some((4, 8)));
        app.apply(UiMsg::Outcome("read done".into()));
        assert_eq!(app.progress, None);
        assert!(app.log.iter().any(|l| l == "read done"));
    }

    #[test]
    fn apply_bad_contact_marks_pins() {
        let mut app = App::new(None);
        app.apply(UiMsg::BadContact(vec![3, 40]));
        assert_eq!(app.pins[2], PinState::Bad);
        assert_eq!(app.pins[39], PinState::Bad);
        assert_eq!(app.pins[0], PinState::Good);
        assert_eq!(app.pins[19], PinState::Good);
    }

    #[test]
    fn reporter_feeds_the_app_channel() {
        use minipro_core::report::{Event, Outcome, Reporter};
        let mut app = App::new(None);
        let mut rep = app.reporter();
        rep.event(&Event::Progress { done: 1, total: 2 });
        rep.finish(&Outcome::Ok { op: "erase" });
        app.drain_events();
        assert!(app.log.iter().any(|l| l == "erase: ok"));
    }

    #[test]
    fn zif_lines_align_forty_pins() {
        let pins = [PinState::Unknown; 40];
        let lines = zif_lines(&pins);
        assert_eq!(lines.len(), 3); // top row, bottom row, legend
        assert_eq!(lines[0].spans.len(), 20);
        assert_eq!(lines[1].spans.len(), 20);
        // Bottom row runs 40 → 21 so pin 40 sits under pin 1.
        assert_eq!(lines[1].spans[0].content.trim(), "40");
        assert_eq!(lines[1].spans[19].content.trim(), "21");
    }

    #[test]
    fn hex_rows_format_offset_hex_ascii() {
        let mut bytes = b"Hello, world!".to_vec();
        bytes.extend_from_slice(&[0x00, 0xff, 0x41]);
        let rows = hex_rows(&bytes, 10);
        assert_eq!(rows.len(), 1);
        assert!(rows[0].starts_with("000000  48 65 6c 6c 6f"));
        assert!(rows[0].ends_with("|Hello, world!..A|"));
        // Cap respected.
        let big = vec![0u8; 16 * 50];
        assert_eq!(hex_rows(&big, 10).len(), 10);
    }

    #[test]
    fn search_without_db_stays_empty() {
        let mut app = App::new(None);
        app.set_search("W25Q64".into());
        assert!(app.hits.is_empty());
        assert_eq!(app.hit_total, 0);
    }

    #[test]
    fn keys_edit_search_and_quit() {
        let mut app = App::new(None);
        app.on_key(KeyCode::Char('w'));
        app.on_key(KeyCode::Char('x'));
        app.on_key(KeyCode::Backspace);
        assert_eq!(app.search, "w");
        // 'q' is a search character while the search box is non-empty.
        app.on_key(KeyCode::Char('q'));
        assert!(!app.quit);
        assert_eq!(app.search, "wq");
        app.on_key(KeyCode::Esc);
        assert!(app.quit);
    }

    /// The TUI read path must load the FPGA algorithm before `begin()` (the F7
    /// gap). `resolve_selected_device` returns a device with `algorithm` set.
    #[test]
    fn resolve_selected_device_loads_algorithm() {
        use minipro_core::device::{Algorithm, Device};
        use minipro_core::error::{FwVersion, Result};
        use minipro_db::Search;

        struct MockDb {
            dev: Device,
        }
        impl ChipDb for MockDb {
            fn get(&self, name: &str) -> Option<&Device> {
                (name == self.dev.name).then_some(&self.dev)
            }
            fn search(&self, _q: &str, _l: usize) -> Search<'_> {
                Search {
                    total: 0,
                    hits: Vec::new(),
                    truncated: false,
                }
            }
            fn firmware_target(&self) -> FwVersion {
                FwVersion(0x0111)
            }
            fn load_algorithm(&self, _dev: &Device) -> Result<Option<Algorithm>> {
                Ok(Some(Algorithm {
                    name: "ALG".into(),
                    bitstream: vec![1, 2, 3],
                }))
            }
        }

        let dev = Device {
            name: "CHIP@DIP8".into(),
            ..Device::default()
        };
        let mut app = App::new(None);
        app.db = Some(Box::new(MockDb { dev }));
        app.hits = vec!["CHIP@DIP8".into()];
        app.list_state.select(Some(0));

        let (resolved, fw) = app.resolve_selected_device().expect("resolve");
        assert_eq!(fw, FwVersion(0x0111));
        assert_eq!(
            resolved.algorithm.expect("algorithm loaded").bitstream,
            vec![1, 2, 3],
            "TUI must load the bitstream so begin() works on FPGA devices",
        );

        // No selection is a graceful user-facing error, not a panic.
        app.list_state.select(None);
        assert!(app.resolve_selected_device().is_err());
    }
}
