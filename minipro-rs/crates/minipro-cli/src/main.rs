//! `minipro` — the CLI entry point.
//!
//! Selects one of three output modes and dispatches a subcommand. The real CLI
//! uses `clap` derive; this scaffold parses `std::env::args` just enough to show
//! the mode-selection skeleton and wire the pieces together.

mod reporters;

use minipro_core::report::Reporter;
use reporters::{HumanReporter, JsonReporter, TuiReporter};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode { Human, Json, Tui }

fn select_mode(args: &[String]) -> Mode {
    // Precedence: `tui` subcommand > --json/env > default human.
    if args.iter().any(|a| a == "tui") {
        Mode::Tui
    } else if args.iter().any(|a| a == "--json")
        || std::env::var("MINIPRO_OUTPUT").as_deref() == Ok("json")
    {
        Mode::Json
    } else {
        Mode::Human
    }
}

fn reporter_for(mode: Mode) -> Box<dyn Reporter> {
    match mode {
        Mode::Human => Box::new(HumanReporter),
        Mode::Json => Box::new(JsonReporter),
        Mode::Tui => Box::new(TuiReporter),
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mode = select_mode(&args);
    let _reporter = reporter_for(mode);

    // Real flow (see docs/rust-trait-model.md §7):
    //   let tx   = minipro_usb::UsbTransport::open(0xA466, 0x1A86)?;
    //   let prog = minipro_proto::detect(Box::new(tx))?;
    //   let db   = minipro_db::CompiledDb::embedded()?;
    //   let dev  = db.get(&chip)...;
    //   let s    = prog.begin(dev)?;
    //   let img  = minipro_core::ops::read_verified(&mut *prog, &s, region, &mut *reporter)?;
    //   reporter.finish(&outcome);

    eprintln!("minipro-rs scaffold — mode: {mode:?} (operations are stubs)");
}
