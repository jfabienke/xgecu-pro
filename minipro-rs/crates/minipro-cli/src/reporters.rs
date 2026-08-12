//! The three [`Reporter`] renderers. All consume the identical event stream from
//! `minipro-core`; they differ only in presentation. See
//! `docs/rust-redesign.md` (output modes) for the design.

use minipro_core::report::{Event, Outcome, Reporter};

/// 1. Human console: colored, progress bars, hints. (Real impl uses
///    `indicatif` + `anstream`; here it just prints to stderr/stdout.)
pub struct HumanReporter;
impl Reporter for HumanReporter {
    fn event(&mut self, ev: &Event) {
        match ev {
            Event::Progress { done, total } => eprintln!("  {done}/{total}"),
            Event::Note(m) => eprintln!("{m}"),
            Event::Warn(_) => eprintln!("warning"),
        }
    }
    fn finish(&mut self, out: &Outcome) {
        match out {
            Outcome::Read { device, bytes, .. } => println!("read {device}: {bytes} bytes"),
            Outcome::Info { model, firmware, .. } => println!("{model} fw {firmware}"),
            Outcome::Ok { op } => println!("{op}: ok"),
        }
    }
}

/// 2. JSON: token-frugal NDJSON. Progress goes to stderr (kept out of the token
///    budget); only the terminal Outcome lands on stdout. The real impl uses
///    `serde_json`; this stub hand-writes the compact objects to show the shape.
pub struct JsonReporter;
impl Reporter for JsonReporter {
    fn event(&mut self, ev: &Event) {
        // Diagnostics to stderr so stdout stays a clean result stream.
        if let Event::Progress { done, total } = ev {
            eprintln!("{{\"progress\":{done},\"total\":{total}}}");
        }
    }
    fn finish(&mut self, out: &Outcome) {
        match out {
            Outcome::Read { device, bytes, crc32, stable, reads, .. } => println!(
                "{{\"op\":\"read\",\"ok\":true,\"dev\":\"{device}\",\"bytes\":{bytes},\"crc32\":\"{crc32:08x}\",\"reads\":{reads},\"stable\":{stable}}}"
            ),
            Outcome::Info { model, firmware, .. } => {
                println!("{{\"op\":\"info\",\"ok\":true,\"model\":\"{model}\",\"fw\":\"{firmware}\"}}")
            }
            Outcome::Ok { op } => println!("{{\"op\":\"{op}\",\"ok\":true}}"),
        }
    }
}

/// 3. TUI: routes the same events into `ratatui` widgets (status, chip browser,
///    live ZIF contact map, hex view). Stub — the real impl owns the terminal.
pub struct TuiReporter;
impl Reporter for TuiReporter {
    fn event(&mut self, _ev: &Event) { /* update widget state */ }
    fn finish(&mut self, _out: &Outcome) { /* final panel */ }
}
