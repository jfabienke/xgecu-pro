//! The three [`Reporter`] renderers. All consume the identical event stream
//! from `minipro-core`; they differ only in presentation. See
//! `docs/rust-redesign.md` ("Output modes") for the design.
//!
//! - [`HumanReporter`] — colored console with an `indicatif` progress bar.
//! - [`JsonReporter`] — token-frugal NDJSON: the terminal `Outcome` on stdout,
//!   progress/warn events on stderr (kept out of an agent's token budget).
//! - [`TuiReporter`] — forwards owned event snapshots over a channel to the
//!   `ratatui` render loop in [`crate::tui`].

use std::io::Write;
use std::sync::mpsc::Sender;

use indicatif::{ProgressBar, ProgressStyle};
use minipro_core::error::Error;
use minipro_core::report::{Event, Outcome, Reporter, Warning};
use minipro_core::transport::LinkSpeed;
use minipro_db::Search;
use owo_colors::OwoColorize;

/// Human-readable link-speed label (the JSON schema uses `fs`/`hs`/`ss`).
pub fn link_label(link: LinkSpeed) -> &'static str {
    match link {
        LinkSpeed::Full => "Full Speed (USB 1.1)",
        LinkSpeed::High => "High Speed (USB 2.0)",
        LinkSpeed::Super => "SuperSpeed (USB 3.x)",
    }
}

fn hex_string(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn warning_text(w: &Warning) -> String {
    match w {
        Warning::FirmwareMismatch => {
            "device firmware does not match the bitstream database target".to_string()
        }
        Warning::BadContact(pins) => format!("bad contact on pins {pins:?}"),
        Warning::ChipIdMismatch { expected, got } => {
            format!("chip id mismatch: expected {expected:04x}, got {got:04x}")
        }
    }
}

// ---------------------------------------------------------------------------
// 1. Human console
// ---------------------------------------------------------------------------

/// Colored console output: an `indicatif` progress bar for `Progress` events,
/// `owo-colors` styling routed through `anstream` (which honors `NO_COLOR` and
/// strips ANSI when stderr/stdout is not a terminal), and `comfy-table` for
/// the tabular commands. Diagnostics go to stderr; results to stdout.
pub struct HumanReporter {
    bar: Option<ProgressBar>,
}

impl HumanReporter {
    pub fn new() -> Self {
        HumanReporter { bar: None }
    }

    /// Print a diagnostic line without garbling an active progress bar.
    fn diag(&self, line: &str) {
        match &self.bar {
            Some(bar) if !bar.is_finished() => bar.println(line),
            _ => anstream::eprintln!("{line}"),
        }
    }
}

impl Default for HumanReporter {
    fn default() -> Self {
        Self::new()
    }
}

impl Reporter for HumanReporter {
    fn event(&mut self, ev: &Event) {
        match ev {
            Event::Progress { done, total } => {
                let bar = self.bar.get_or_insert_with(|| {
                    let bar = ProgressBar::new(*total);
                    bar.set_style(
                        ProgressStyle::with_template(
                            "  {bar:32.cyan/blue} {bytes}/{total_bytes} ({eta})",
                        )
                        .expect("static progress template is valid")
                        .progress_chars("█▉▊▋▌▍▎▏ "),
                    );
                    bar
                });
                bar.set_length(*total);
                bar.set_position(*done);
            }
            Event::Warn(w) => {
                let msg = format!("{} {}", "warning:".yellow().bold(), warning_text(w));
                self.diag(&msg);
            }
            Event::Note(n) => self.diag(n),
        }
    }

    fn finish(&mut self, out: &Outcome) {
        if let Some(bar) = self.bar.take() {
            bar.finish_and_clear();
        }
        match out {
            Outcome::Read { device, bytes, crc32, sha256, reads, stable, link } => {
                let stability = if *stable {
                    format!("{}", "stable".green())
                } else {
                    format!("{}", "UNSTABLE".red().bold())
                };
                anstream::println!(
                    "{} read {} — {} bytes",
                    "✓".green().bold(),
                    device.bold(),
                    bytes
                );
                anstream::println!("  crc32   {crc32:08x}");
                anstream::println!("  sha256  {}", hex_string(sha256));
                anstream::println!("  reads   {reads} ({stability})");
                anstream::println!("  link    {}", link_label(*link));
            }
            Outcome::Info { model, firmware, firmware_expected, link, vcc } => {
                let mut table = comfy_table::Table::new();
                table.load_style(comfy_table::presets::UTF8_FULL_CONDENSED);
                table.set_header(vec!["programmer", "value"]);
                let fw = if firmware == firmware_expected {
                    firmware.clone()
                } else {
                    format!("{firmware} (bitstreams target {firmware_expected})")
                };
                table.add_row(vec!["model", model]);
                table.add_row(vec!["firmware", &fw]);
                table.add_row(vec!["link", link_label(*link)]);
                table.add_row(vec!["vcc", &format!("{vcc:.1} V")]);
                anstream::println!("{table}");
                if firmware != firmware_expected {
                    anstream::eprintln!(
                        "{} {}",
                        "warning:".yellow().bold(),
                        "device firmware differs from the bitstream database target"
                    );
                }
            }
            Outcome::Ok { op } => {
                anstream::println!("{} {op}: ok", "✓".green().bold());
            }
        }
    }
}

/// Render an error (with its remediation hint) for the human console.
pub fn human_error(err: &Error) {
    anstream::eprintln!("{} {err}", "error:".red().bold());
    if let Some(hint) = err.hint() {
        anstream::eprintln!("{} {hint}", "hint:".yellow().bold());
    }
}

/// `comfy-table` rendering of a chip-DB search (bounded and counted, per the
/// design — never all 34k devices).
pub fn search_table(query: &str, search: &Search<'_>) -> String {
    let mut table = comfy_table::Table::new();
    table.load_style(comfy_table::presets::UTF8_FULL_CONDENSED);
    table.set_header(vec!["name", "package", "pins", "code", "data", "chip id"]);
    for dev in &search.hits {
        table.add_row(vec![
            dev.name.clone(),
            dev.package.name.clone(),
            dev.package.pin_count.to_string(),
            dev.code_size.to_string(),
            dev.data_size.to_string(),
            format!("{:04x}", dev.chip_id),
        ]);
    }
    let mut out = format!("{table}\n");
    out.push_str(&format!(
        "{} match(es) for {query:?}, showing {}",
        search.total,
        search.hits.len()
    ));
    if search.truncated {
        out.push_str(" (truncated — raise --limit)");
    }
    out
}

// ---------------------------------------------------------------------------
// 2. JSON (NDJSON)
// ---------------------------------------------------------------------------

/// Token-frugal NDJSON. The terminal `Outcome` is one compact line on stdout;
/// progress/warn/note events stream to stderr so they never spend an agent's
/// tokens. Serialization is `serde_json` over the core's own `Serialize`
/// impls, so the schema is exactly the one documented in `rust-redesign.md`.
pub struct JsonReporter {
    out: Box<dyn Write + Send>,
    err: Box<dyn Write + Send>,
}

impl JsonReporter {
    pub fn new() -> Self {
        Self::with_writers(Box::new(std::io::stdout()), Box::new(std::io::stderr()))
    }

    /// Inject writers (used by tests to capture the streams).
    pub fn with_writers(out: Box<dyn Write + Send>, err: Box<dyn Write + Send>) -> Self {
        JsonReporter { out, err }
    }
}

impl Default for JsonReporter {
    fn default() -> Self {
        Self::new()
    }
}

impl Reporter for JsonReporter {
    fn event(&mut self, ev: &Event) {
        // Events are diagnostics: stderr, one compact object per line.
        if let Ok(line) = serde_json::to_string(ev) {
            let _ = writeln!(self.err, "{line}");
        }
    }

    fn finish(&mut self, out: &Outcome) {
        // The final Outcome is the result: stdout, exactly one line.
        if let Ok(line) = serde_json::to_string(out) {
            let _ = writeln!(self.out, "{line}");
        }
        let _ = self.out.flush();
    }
}

/// The JSON error line: `{"op":…,"ok":false,"code":…,"msg":…,"hint":…}`.
/// `code` is the machine-stable [`Error::code`] an agent branches on.
pub fn json_error_line(op: &str, err: &Error) -> String {
    let mut map = serde_json::Map::new();
    map.insert("op".into(), op.into());
    map.insert("ok".into(), false.into());
    map.insert("code".into(), err.code().into());
    map.insert("msg".into(), err.to_string().into());
    if let Some(hint) = err.hint() {
        map.insert("hint".into(), hint.into());
    }
    serde_json::Value::Object(map).to_string()
}

/// The JSON search line: bounded and counted, never a full dump.
pub fn search_json_line(query: &str, search: &Search<'_>) -> String {
    let hits: Vec<&str> = search.hits.iter().map(|d| d.name.as_str()).collect();
    serde_json::json!({
        "op": "search",
        "ok": true,
        "q": query,
        "n": search.total,
        "shown": hits.len(),
        "more": search.truncated,
        "hits": hits,
    })
    .to_string()
}

// ---------------------------------------------------------------------------
// 3. TUI (channel into the ratatui render loop)
// ---------------------------------------------------------------------------

/// Owned snapshot of an [`Event`]/[`Outcome`], sendable across the worker →
/// render-thread channel (the core types borrow / are not `Clone`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UiMsg {
    Progress { done: u64, total: u64 },
    Note(String),
    Warn(String),
    /// Open pins reported by the contact check — drives the live ZIF map.
    BadContact(Vec<u8>),
    /// Terminal outcome, pre-rendered to a summary line.
    Outcome(String),
    /// Dump bytes for the hex view.
    Hex(Vec<u8>),
    /// The worker thread finished and released the programmer.
    OpDone,
}

/// Bridges the core's event stream onto a channel consumed by the `ratatui`
/// app ([`crate::tui`]). `Sender` is `Send`, so ops can run on a worker thread
/// while the render loop owns the terminal — exactly the design's threading
/// model.
pub struct TuiReporter {
    tx: Sender<UiMsg>,
}

impl TuiReporter {
    /// Attach a reporter to a render loop's channel (see `tui::App::reporter`).
    pub fn from_sender(tx: Sender<UiMsg>) -> Self {
        TuiReporter { tx }
    }
}

/// One-line summary of an outcome for the TUI log pane.
pub fn outcome_summary(out: &Outcome) -> String {
    match out {
        Outcome::Read { device, bytes, crc32, stable, .. } => format!(
            "read {device}: {bytes} bytes, crc32 {crc32:08x}, {}",
            if *stable { "stable" } else { "UNSTABLE" }
        ),
        Outcome::Info { model, firmware, .. } => format!("{model} fw {firmware}"),
        Outcome::Ok { op } => format!("{op}: ok"),
    }
}

impl Reporter for TuiReporter {
    fn event(&mut self, ev: &Event) {
        let msg = match ev {
            Event::Progress { done, total } => UiMsg::Progress { done: *done, total: *total },
            Event::Note(n) => UiMsg::Note(n.to_string()),
            Event::Warn(Warning::BadContact(pins)) => UiMsg::BadContact(pins.clone()),
            Event::Warn(w) => UiMsg::Warn(warning_text(w)),
        };
        let _ = self.tx.send(msg); // render loop gone -> drop silently
    }

    fn finish(&mut self, out: &Outcome) {
        let _ = self.tx.send(UiMsg::Outcome(outcome_summary(out)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use minipro_core::device::{Device, Package};
    use minipro_core::error::FwVersion;
    use std::sync::{Arc, Mutex};

    /// A `Write` handle tests can keep a reference to after moving it into the
    /// reporter.
    #[derive(Clone, Default)]
    struct Shared(Arc<Mutex<Vec<u8>>>);
    impl Write for Shared {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    impl Shared {
        fn take_string(&self) -> String {
            String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
        }
    }

    fn fake_device(name: &str) -> Device {
        Device {
            name: name.into(),
            protocol_id: 1,
            code_size: 32768,
            data_size: 0,
            page_size: 64,
            chip_id: 0x208d,
            chip_id_bytes: 2,
            package: Package { pin_count: 28, name: "DIP28".into() },
            algorithm: None,
            fw_target: FwVersion(0x00_01_11),
            ..Device::default()
        }
    }

    #[test]
    fn json_reporter_routes_events_to_stderr_and_outcome_to_stdout() {
        let out = Shared::default();
        let err = Shared::default();
        let mut rep = JsonReporter::with_writers(Box::new(out.clone()), Box::new(err.clone()));

        rep.event(&Event::Progress { done: 4, total: 10 });
        rep.event(&Event::Warn(Warning::BadContact(vec![3, 4])));
        rep.finish(&Outcome::Ok { op: "erase" });

        assert_eq!(out.take_string(), "{\"op\":\"erase\",\"ok\":true}\n");
        let err_lines = err.take_string();
        let mut lines = err_lines.lines();
        assert_eq!(lines.next().unwrap(), r#"{"progress":{"done":4,"total":10}}"#);
        assert_eq!(lines.next().unwrap(), r#"{"warn":{"bad_contact":[3,4]}}"#);
        assert!(lines.next().is_none());
    }

    #[test]
    fn json_outcome_read_is_compact_documented_schema() {
        let out = Shared::default();
        let mut rep =
            JsonReporter::with_writers(Box::new(out.clone()), Box::new(Shared::default()));
        rep.finish(&Outcome::Read {
            device: "M27C256B@DIP28".into(),
            bytes: 32768,
            crc32: 0x5931_8a17,
            sha256: [0xab; 32],
            reads: 2,
            stable: true,
            link: LinkSpeed::High,
        });
        let line = out.take_string();
        assert_eq!(line.lines().count(), 1);
        let v: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(v["op"], "read");
        assert_eq!(v["crc32"], "59318a17");
        assert_eq!(v["link"], "hs");
        assert!(!line.contains(": "), "must be compact, not pretty-printed");
    }

    #[test]
    fn json_error_line_carries_stable_code_and_hint() {
        let err = Error::BadContact(vec![3, 4]);
        let line = json_error_line("read", &err);
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["op"], "read");
        assert_eq!(v["ok"], false);
        assert_eq!(v["code"], "bad_contact");
        assert!(v["hint"].as_str().unwrap().contains("--skip-pincheck"));
    }

    #[test]
    fn json_error_line_omits_absent_hint() {
        let line = json_error_line("write", &Error::Protocol);
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["code"], "protocol");
        assert!(v.get("hint").is_none());
    }

    #[test]
    fn search_json_line_is_bounded_and_counted() {
        let devs = [fake_device("M27C256B@DIP28"), fake_device("M27C256B@PLCC32")];
        let search = Search { total: 70, hits: devs.iter().collect(), truncated: true };
        let v: serde_json::Value =
            serde_json::from_str(&search_json_line("27C256", &search)).unwrap();
        assert_eq!(v["op"], "search");
        assert_eq!(v["n"], 70);
        assert_eq!(v["shown"], 2);
        assert_eq!(v["more"], true);
        assert_eq!(v["hits"][0], "M27C256B@DIP28");
    }

    #[test]
    fn search_table_lists_hits_and_counts() {
        let devs = [fake_device("M27C256B@DIP28")];
        let search = Search { total: 1, hits: devs.iter().collect(), truncated: false };
        let table = search_table("27C256", &search);
        assert!(table.contains("M27C256B@DIP28"));
        assert!(table.contains("DIP28"));
        assert!(table.contains("1 match(es)"));
        assert!(!table.contains("truncated"));
    }

    #[test]
    fn tui_reporter_maps_events_to_owned_messages() {
        let (tx, rx) = std::sync::mpsc::channel();
        let mut rep = TuiReporter::from_sender(tx);
        rep.event(&Event::Progress { done: 1, total: 2 });
        rep.event(&Event::Warn(Warning::BadContact(vec![7])));
        rep.event(&Event::Note("hello".into()));
        rep.finish(&Outcome::Ok { op: "erase" });

        assert_eq!(rx.recv().unwrap(), UiMsg::Progress { done: 1, total: 2 });
        assert_eq!(rx.recv().unwrap(), UiMsg::BadContact(vec![7]));
        assert_eq!(rx.recv().unwrap(), UiMsg::Note("hello".into()));
        assert_eq!(rx.recv().unwrap(), UiMsg::Outcome("erase: ok".into()));
    }

    #[test]
    fn human_reporter_smoke() {
        // No panics across the event/outcome surface; visual output is not
        // asserted (progress bar draws to stderr only when it is a TTY).
        let mut rep = HumanReporter::new();
        rep.event(&Event::Progress { done: 0, total: 8 });
        rep.event(&Event::Warn(Warning::FirmwareMismatch));
        rep.event(&Event::Note("note".into()));
        rep.event(&Event::Progress { done: 8, total: 8 });
        rep.finish(&Outcome::Ok { op: "erase" });
    }
}
