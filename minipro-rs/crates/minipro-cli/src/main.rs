//! `minipro` — the CLI entry point.
//!
//! Selects one of three output modes and dispatches a subcommand. Mode
//! precedence (see `docs/rust-redesign.md`, "Output modes"): the `tui`
//! subcommand wins, then `--json` / `MINIPRO_OUTPUT=json`, then the default
//! human console. Old-style `-p`/`-r`/`-w` flags are kept as aliases for the
//! `read`/`write` subcommands.

mod reporters;
mod tui;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{CommandFactory, Parser, Subcommand};
use minipro_core::device::{EraseKind, Image, Region};
use minipro_core::error::{Error, Result};
use minipro_core::ops;
use minipro_core::programmer::Txn;
use minipro_core::report::{Event, Outcome, Reporter, Warning};
use minipro_core::Programmer;
use minipro_db::{CachedDb, ChipDb, DllDb, XmlDb};
use reporters::{HumanReporter, JsonReporter};

/// The XGecu T76's USB identity (see `UsbTransport::open`).
const T76_VID: u16 = 0xA466;
const T76_PID: u16 = 0x1A86;

#[derive(Parser, Debug)]
#[command(
    name = "minipro",
    version,
    about = "CLI for XGecu USB device programmers (T76 first)",
    long_about = None
)]
struct Cli {
    /// Emit NDJSON: the final outcome on stdout, events on stderr
    /// (equivalently: MINIPRO_OUTPUT=json)
    #[arg(long, global = true)]
    json: bool,

    /// Chip database directory (infoic.xml / algorithm.xml / InfoICT76.dll)
    #[arg(long, global = true, env = "MINIPRO_DB_DIR", value_name = "DIR")]
    db: Option<PathBuf>,

    /// Provision the native DB from a mirror over HTTP(S), opt-in. Fetches
    /// InfoICT76.dll (once) + .alg files (on demand) into the --db cache dir
    /// (or a default cache). Serves the *extracted* files:
    /// `<url>/InfoICT76.dll`, `<url>/algoT76/<algo>.alg`.
    #[arg(long, global = true, env = "MINIPRO_DB_URL", value_name = "URL")]
    db_url: Option<String>,

    /// Select chip (legacy flag; pairs with -r/-w)
    #[arg(short = 'p', value_name = "CHIP", help_heading = "Legacy flags")]
    legacy_chip: Option<String>,

    /// Read chip into FILE (legacy alias for `minipro read`)
    #[arg(short = 'r', value_name = "FILE", help_heading = "Legacy flags", requires = "legacy_chip")]
    legacy_read: Option<PathBuf>,

    /// Write FILE to chip (legacy alias for `minipro write`)
    #[arg(
        short = 'w',
        value_name = "FILE",
        help_heading = "Legacy flags",
        requires = "legacy_chip",
        conflicts_with = "legacy_read"
    )]
    legacy_write: Option<PathBuf>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand, Debug, Clone)]
enum Command {
    /// Read a chip into FILE, with re-read verification (crc32/sha256/stable)
    Read {
        /// Chip name, e.g. "M27C256B@DIP28"
        chip: String,
        /// Output file for the dump
        file: PathBuf,
        /// Proceed despite a chip-id mismatch
        #[arg(long)]
        force: bool,
        /// Skip the pin-contact check
        #[arg(long)]
        skip_pincheck: bool,
    },
    /// Write FILE to a chip, verified by read-back
    Write {
        /// Chip name, e.g. "M27C256B@DIP28"
        chip: String,
        /// Image file to program
        file: PathBuf,
        /// Proceed despite a chip-id mismatch
        #[arg(long)]
        force: bool,
        /// Skip the pin-contact check
        #[arg(long)]
        skip_pincheck: bool,
    },
    /// Erase the whole chip
    Erase {
        /// Chip name, e.g. "W25Q64BV@SOIC8"
        chip: String,
    },
    /// Show programmer identity and firmware status
    Info,
    /// Search the chip database (capped and counted)
    Search {
        /// Substring to match against device names
        query: String,
        /// Maximum number of hits to show
        #[arg(long, default_value_t = 10)]
        limit: usize,
    },
    /// Interactive terminal UI (chip browser, ZIF contact map, hex view)
    Tui,
    /// Read the seated chip's electronic id and match it in the database.
    ///
    /// Parallel chips need a bitstream loaded before they can be talked to at
    /// all, so `--like` names a same-package chip to establish the socket /
    /// algorithm family; the id that comes back is the *actual* seated chip's.
    Detect {
        /// A chip of the same package to borrow the socket/algorithm from
        #[arg(long, default_value = "M27C256B@DIP28")]
        like: String,
    },
}

impl Command {
    /// The `op` string used in JSON error lines.
    fn op_name(&self) -> &'static str {
        match self {
            Command::Read { .. } => "read",
            Command::Write { .. } => "write",
            Command::Erase { .. } => "erase",
            Command::Info => "info",
            Command::Search { .. } => "search",
            Command::Tui => "tui",
            Command::Detect { .. } => "detect",
        }
    }
}

impl Cli {
    /// Resolve subcommand vs. legacy `-p/-r/-w` flags into one command.
    fn resolve_command(&self) -> Result<Option<Command>> {
        if let Some(cmd) = &self.command {
            if self.legacy_chip.is_some() {
                return Err(Error::Format(
                    "use either a subcommand or the legacy -p/-r/-w flags, not both".into(),
                ));
            }
            return Ok(Some(cmd.clone()));
        }
        match (&self.legacy_chip, &self.legacy_read, &self.legacy_write) {
            (Some(chip), Some(file), None) => Ok(Some(Command::Read {
                chip: chip.clone(),
                file: file.clone(),
                force: false,
                skip_pincheck: false,
            })),
            (Some(chip), None, Some(file)) => Ok(Some(Command::Write {
                chip: chip.clone(),
                file: file.clone(),
                force: false,
                skip_pincheck: false,
            })),
            (Some(_), None, None) => {
                Err(Error::Format("-p needs -r FILE (read) or -w FILE (write)".into()))
            }
            _ => Ok(None),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Human,
    Json,
    Tui,
}

/// Precedence: `tui` subcommand > `--json` / `MINIPRO_OUTPUT=json` > human.
fn select_mode(command: Option<&Command>, json_flag: bool, env_output: Option<&str>) -> Mode {
    if matches!(command, Some(Command::Tui)) {
        Mode::Tui
    } else if json_flag || env_output == Some("json") {
        Mode::Json
    } else {
        Mode::Human
    }
}

fn reporter_for(mode: Mode) -> Box<dyn Reporter> {
    match mode {
        Mode::Json => Box::new(JsonReporter::new()),
        // The TUI builds its own channel-backed reporter inside `tui::run`;
        // human is the fallback for anything else.
        Mode::Human | Mode::Tui => Box::new(HumanReporter::new()),
    }
}

fn render_error(mode: Mode, op: &'static str, err: &Error) {
    match mode {
        Mode::Json => println!("{}", reporters::json_error_line(op, err)),
        Mode::Human | Mode::Tui => reporters::human_error(err),
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    if let Some(u) = cli.db_url.as_deref() {
        std::env::set_var("MINIPRO_DB_URL", u);
    }
    let env_output = std::env::var("MINIPRO_OUTPUT").ok();
    let mode = select_mode(cli.command.as_ref(), cli.json, env_output.as_deref());

    let command = match cli.resolve_command() {
        Ok(Some(cmd)) => cmd,
        Ok(None) => {
            let _ = Cli::command().print_help();
            return ExitCode::FAILURE;
        }
        Err(e) => {
            render_error(mode, "cli", &e);
            return ExitCode::FAILURE;
        }
    };

    let op = command.op_name();
    let db_dir = cli.db.as_deref();
    let result = match &command {
        Command::Tui => tui::run(cli.db.clone()),
        Command::Search { query, limit } => run_search(db_dir, query, *limit, mode),
        Command::Read { chip, file, force, skip_pincheck } => {
            run_read(db_dir, chip, file, *force, *skip_pincheck, &mut *reporter_for(mode))
        }
        Command::Write { chip, file, force, skip_pincheck } => {
            run_write(db_dir, chip, file, *force, *skip_pincheck, &mut *reporter_for(mode))
        }
        Command::Erase { chip } => run_erase(db_dir, chip, &mut *reporter_for(mode)),
        Command::Info => run_info(db_dir, &mut *reporter_for(mode)),
        Command::Detect { like } => run_detect(db_dir, like, mode),
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            render_error(mode, op, &e);
            ExitCode::FAILURE
        }
    }
}

// ---------------------------------------------------------------------------
// Wiring: transport -> programmer -> db -> ops
// ---------------------------------------------------------------------------

/// Open the USB transport and detect the attached programmer.
/// With nothing plugged in this returns the transport's "no device" error.
pub(crate) fn open_programmer() -> Result<Box<dyn Programmer>> {
    let tx = minipro_usb::UsbTransport::open(T76_VID, T76_PID)?;
    tx.check_link()?; // macOS + SuperSpeed diagnosis, not a bare I/O error
    minipro_proto::detect(Box::new(tx))
}

/// Load the chip database. Boxed so callers stay backend-agnostic once a
/// compiled/embedded backend implements `ChipDb` too.
/// Cache directory for a provisioned (`--db-url`) database.
fn default_cache_dir() -> PathBuf {
    if let Ok(x) = std::env::var("XDG_CACHE_HOME") {
        return PathBuf::from(x).join("minipro");
    }
    if let Ok(h) = std::env::var("HOME") {
        return PathBuf::from(h).join(".cache/minipro");
    }
    std::env::temp_dir().join("minipro-cache")
}

/// Load the chip database. Precedence: `--db-url` (mirror, cached) > a `--db`
/// directory with `InfoICT76.dll` (native `DllDb`) > `infoic.xml` (`XmlDb`).
fn load_db(dir: Option<&Path>) -> Result<Box<dyn ChipDb>> {
    if let Ok(url) = std::env::var("MINIPRO_DB_URL") {
        if !url.is_empty() {
            let cache = dir.map(Path::to_path_buf).unwrap_or_else(default_cache_dir);
            return Ok(Box::new(CachedDb::provision(&url, &cache, None)?));
        }
    }
    let dir = dir.ok_or_else(|| {
        Error::Format("no chip database: pass --db <dir>, set MINIPRO_DB_DIR, or --db-url <mirror>".into())
    })?;
    if dir.join("InfoICT76.dll").is_file() {
        Ok(Box::new(DllDb::load(dir)?))
    } else {
        Ok(Box::new(XmlDb::load(dir)?))
    }
}

/// Look up a chip and attach its FPGA bitstream from `algorithm.xml` (the DB
/// stores chip parameters and bitstreams separately; `begin` needs both).
fn lookup_device(db: &dyn ChipDb, chip: &str) -> Result<minipro_core::device::Device> {
    let mut dev = db
        .get(chip)
        .cloned()
        .ok_or(Error::Unsupported("unknown chip (try `minipro search`)"))?;
    dev.algorithm = db.load_algorithm(&dev)?;
    Ok(dev)
}

/// Warn (typed, not printf) when the programmer firmware differs from the
/// version the DB's bitstreams target.
fn warn_firmware(prog: &dyn Programmer, db: &dyn ChipDb, rep: &mut dyn Reporter) {
    if prog.info().firmware != db.firmware_target() {
        rep.event(&Event::Warn(Warning::FirmwareMismatch));
    }
}

/// Contact check (advisory hardware permitting): errors with `BadContact`
/// unless skipped; programmers without pin-test support silently pass.
fn pincheck(prog: &mut dyn Programmer, s: &minipro_core::Session, skip: bool, rep: &mut dyn Reporter) -> Result<()> {
    if skip {
        return Ok(());
    }
    let Some(pins) = prog.pins() else { return Ok(()) };
    let open = pins.contact_check(s)?;
    if open.is_empty() {
        return Ok(());
    }
    rep.event(&Event::Warn(Warning::BadContact(open.clone())));
    Err(Error::BadContact(open))
}

/// Compare the electronic id against the DB entry; `--force` downgrades a
/// mismatch to a warning.
fn check_chip_id(
    prog: &mut dyn Programmer,
    s: &minipro_core::Session,
    dev: &minipro_core::device::Device,
    force: bool,
    rep: &mut dyn Reporter,
) -> Result<()> {
    if dev.chip_id_bytes == 0 {
        return Ok(()); // chip has no electronic id
    }
    let id = prog.identify(s)?;
    if id.raw == dev.chip_id {
        return Ok(());
    }
    if force {
        rep.event(&Event::Warn(Warning::ChipIdMismatch { expected: dev.chip_id, got: id.raw }));
        Ok(())
    } else {
        Err(Error::ChipIdMismatch { expected: dev.chip_id, got: id.raw, alias: None })
    }
}

fn run_read(
    db_dir: Option<&Path>,
    chip: &str,
    file: &Path,
    force: bool,
    skip_pincheck: bool,
    rep: &mut dyn Reporter,
) -> Result<()> {
    let db = load_db(db_dir)?;
    let dev = lookup_device(&*db, chip)?;
    let mut prog = open_programmer()?;
    warn_firmware(&*prog, &*db, rep);
    let link = prog.info().link;

    let verified = {
        let mut txn = Txn::begin(&mut *prog, &dev)?; // ends (de-energizes) on drop
        let (p, s) = txn.parts();
        pincheck(p, s, skip_pincheck, rep)?;
        check_chip_id(p, s, &dev, force, rep)?;
        ops::read_verified(p, s, Region::code(&dev), rep)?
    };

    std::fs::write(file, &verified.image.bytes)?;
    rep.finish(&verified.outcome(&dev.name, link));
    Ok(())
}

fn run_write(
    db_dir: Option<&Path>,
    chip: &str,
    file: &Path,
    force: bool,
    skip_pincheck: bool,
    rep: &mut dyn Reporter,
) -> Result<()> {
    let db = load_db(db_dir)?;
    let dev = lookup_device(&*db, chip)?;
    let image = Image { bytes: std::fs::read(file)? };
    let mut prog = open_programmer()?;
    warn_firmware(&*prog, &*db, rep);

    {
        let mut txn = Txn::begin(&mut *prog, &dev)?;
        let (p, s) = txn.parts();
        pincheck(p, s, skip_pincheck, rep)?;
        check_chip_id(p, s, &dev, force, rep)?;
        ops::write_region(p, s, Region::code(&dev), &image, rep)?;
    }

    rep.finish(&Outcome::Ok { op: "write" });
    Ok(())
}

fn run_erase(db_dir: Option<&Path>, chip: &str, rep: &mut dyn Reporter) -> Result<()> {
    let db = load_db(db_dir)?;
    let dev = lookup_device(&*db, chip)?;
    let mut prog = open_programmer()?;
    warn_firmware(&*prog, &*db, rep);

    {
        let mut txn = Txn::begin(&mut *prog, &dev)?;
        let (p, s) = txn.parts();
        let mem = p.memory().ok_or(Error::Unsupported("memory ops"))?;
        mem.erase(s, EraseKind::Chip)?;
    }

    rep.finish(&Outcome::Ok { op: "erase" });
    Ok(())
}

fn run_info(db_dir: Option<&Path>, rep: &mut dyn Reporter) -> Result<()> {
    let prog = open_programmer()?;
    let info = prog.info();
    let firmware = info.firmware.to_string();
    // Without a DB we have no bitstream target to compare against, so the
    // expected version mirrors the device's (i.e. "no known mismatch").
    let firmware_expected = match db_dir {
        Some(dir) => load_db(Some(dir))?.firmware_target().to_string(),
        None => firmware.clone(),
    };
    rep.finish(&Outcome::Info {
        model: info.model.clone(),
        firmware,
        firmware_expected,
        link: info.link,
        vcc: info.voltage,
    });
    Ok(())
}

/// Decode a JEDEC manufacturer id byte to a name (common vendors only).
fn manufacturer_name(id: u8) -> &'static str {
    match id {
        0x01 => "AMD",
        0x04 => "Fujitsu",
        0x1c => "EON",
        0x1e | 0x1f => "Atmel",
        0x20 => "STMicroelectronics",
        0x37 => "AMIC",
        0x89 => "Intel",
        0x9d => "ISSI",
        0xad => "Hynix",
        0xbf => "SST",
        0xc2 => "Macronix",
        0xc8 => "GigaDevice",
        0xda => "Winbond (old)",
        0xec => "Samsung",
        0xef => "Winbond",
        _ => "unknown vendor",
    }
}

/// Read the seated chip's electronic id (through the `--like` socket family) and
/// report which database devices share that id.
fn run_detect(db_dir: Option<&Path>, like: &str, mode: Mode) -> Result<()> {
    let db = load_db(db_dir)?;
    let dev = lookup_device(&*db, like)?;
    let mut prog = open_programmer()?;

    let id = {
        let mut txn = Txn::begin(&mut *prog, &dev)?; // uploads the socket-family bitstream
        let (p, s) = txn.parts();
        p.identify(s)? // reads the ACTUAL chip's signature
    };

    let mut matches: Vec<&str> = if id.bytes == 0 {
        Vec::new()
    } else {
        db.all()
            .iter()
            .filter(|d| d.chip_id_bytes == id.bytes && d.chip_id == id.raw)
            .map(|d| d.name.as_str())
            .collect()
    };
    matches.sort_unstable();
    matches.dedup();

    let width = (id.bytes.max(1) as usize) * 2;
    let mfr = manufacturer_name((id.raw >> (id.bytes.saturating_sub(1) as u32 * 8)) as u8);
    let shown: Vec<&str> = matches.iter().copied().take(12).collect();

    match mode {
        Mode::Json => {
            let arr: Vec<String> = shown.iter().map(|s| format!("{s:?}")).collect();
            println!(
                "{{\"op\":\"detect\",\"ok\":true,\"id\":\"{:0width$x}\",\"bytes\":{},\"manufacturer\":\"{}\",\"n\":{},\"matches\":[{}]}}",
                id.raw,
                id.bytes,
                mfr,
                matches.len(),
                arr.join(",")
            );
        }
        _ => {
            anstream::println!("Electronic id: 0x{:0width$X}  ({} bytes) — {}", id.raw, id.bytes, mfr);
            if matches.is_empty() {
                anstream::println!(
                    "No database match (chip may be blank/mis-seated, or the id isn't in the DB)."
                );
            } else {
                anstream::println!("Matches {} database device(s):", matches.len());
                for name in &shown {
                    anstream::println!("  {name}");
                }
                if matches.len() > shown.len() {
                    anstream::println!("  … and {} more", matches.len() - shown.len());
                }
            }
        }
    }
    Ok(())
}

fn run_search(db_dir: Option<&Path>, query: &str, limit: usize, mode: Mode) -> Result<()> {
    let db = load_db(db_dir)?;
    let found = db.search(query, limit);
    match mode {
        Mode::Json => println!("{}", reporters::search_json_line(query, &found)),
        Mode::Human | Mode::Tui => anstream::println!("{}", reporters::search_table(query, &found)),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Cli {
        Cli::try_parse_from(args).expect("args parse")
    }

    #[test]
    fn clap_definition_is_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn mode_precedence_tui_beats_json() {
        let tui = Command::Tui;
        assert_eq!(select_mode(Some(&tui), true, Some("json")), Mode::Tui);
        assert_eq!(select_mode(Some(&tui), false, None), Mode::Tui);
    }

    #[test]
    fn mode_json_from_flag_or_env() {
        let info = Command::Info;
        assert_eq!(select_mode(Some(&info), true, None), Mode::Json);
        assert_eq!(select_mode(Some(&info), false, Some("json")), Mode::Json);
        assert_eq!(select_mode(Some(&info), false, Some("human")), Mode::Human);
        assert_eq!(select_mode(Some(&info), false, None), Mode::Human);
        assert_eq!(select_mode(None, false, None), Mode::Human);
    }

    #[test]
    fn subcommands_parse() {
        let cli = parse(&["minipro", "read", "M27C256B@DIP28", "dump.bin", "--force"]);
        match cli.resolve_command().unwrap().unwrap() {
            Command::Read { chip, file, force, skip_pincheck } => {
                assert_eq!(chip, "M27C256B@DIP28");
                assert_eq!(file, PathBuf::from("dump.bin"));
                assert!(force);
                assert!(!skip_pincheck);
            }
            other => panic!("expected read, got {other:?}"),
        }
        let cli = parse(&["minipro", "search", "W25Q64", "--limit", "5"]);
        match cli.resolve_command().unwrap().unwrap() {
            Command::Search { query, limit } => {
                assert_eq!(query, "W25Q64");
                assert_eq!(limit, 5);
            }
            other => panic!("expected search, got {other:?}"),
        }
    }

    #[test]
    fn legacy_read_flags_map_to_read_command() {
        let cli = parse(&["minipro", "-p", "M27C256B@DIP28", "-r", "dump.bin"]);
        match cli.resolve_command().unwrap().unwrap() {
            Command::Read { chip, file, .. } => {
                assert_eq!(chip, "M27C256B@DIP28");
                assert_eq!(file, PathBuf::from("dump.bin"));
            }
            other => panic!("expected read, got {other:?}"),
        }
    }

    #[test]
    fn legacy_write_flags_map_to_write_command() {
        let cli = parse(&["minipro", "-p", "AT28C256@DIP28", "-w", "image.bin"]);
        match cli.resolve_command().unwrap().unwrap() {
            Command::Write { chip, file, .. } => {
                assert_eq!(chip, "AT28C256@DIP28");
                assert_eq!(file, PathBuf::from("image.bin"));
            }
            other => panic!("expected write, got {other:?}"),
        }
    }

    #[test]
    fn legacy_flag_misuse_is_rejected() {
        // -r without -p: clap-level `requires`.
        assert!(Cli::try_parse_from(["minipro", "-r", "dump.bin"]).is_err());
        // -r and -w together: clap-level conflict.
        assert!(Cli::try_parse_from(["minipro", "-p", "X", "-r", "a", "-w", "b"]).is_err());
        // -p alone: resolve-level error.
        let cli = parse(&["minipro", "-p", "X"]);
        assert_eq!(cli.resolve_command().unwrap_err().code(), "format");
        // Mixing legacy flags with a subcommand: resolve-level error.
        let cli = parse(&["minipro", "-p", "X", "info"]);
        assert_eq!(cli.resolve_command().unwrap_err().code(), "format");
    }

    #[test]
    fn no_args_resolves_to_none_for_help() {
        let cli = parse(&["minipro"]);
        assert!(cli.resolve_command().unwrap().is_none());
    }

    #[test]
    fn global_json_flag_parses_after_subcommand() {
        let cli = parse(&["minipro", "info", "--json"]);
        assert!(cli.json);
        assert_eq!(select_mode(cli.command.as_ref(), cli.json, None), Mode::Json);
    }

    #[test]
    fn missing_db_is_a_clear_error() {
        let err = match load_db(None) {
            Err(e) => e,
            Ok(_) => panic!("load_db(None) must fail"),
        };
        assert_eq!(err.code(), "format");
        assert!(err.to_string().contains("MINIPRO_DB_DIR"));
    }

    /// End-to-end over the trait surface without hardware: a fake programmer
    /// (like minipro-core's own test double) driven through the same
    /// `run_*`-style flow, rendered by the real JSON reporter.
    #[test]
    fn read_flow_over_fake_programmer_produces_json_outcome() {
        use minipro_core::caps::MemoryOps;
        use minipro_core::device::{BlockReq, ChipId, Device, Package};
        use minipro_core::error::FwVersion;
        use minipro_core::programmer::{Caps, ProgrammerInfo, Session};
        use minipro_core::transport::LinkSpeed;
        use std::io::Write;
        use std::sync::{Arc, Mutex};

        struct Fake {
            info: ProgrammerInfo,
            mem: Vec<u8>,
        }
        impl Programmer for Fake {
            fn info(&self) -> &ProgrammerInfo {
                &self.info
            }
            fn caps(&self) -> Caps {
                Caps::MEMORY
            }
            fn begin(&mut self, dev: &Device) -> Result<Session> {
                Ok(Session { device: dev.clone(), emmc_capacity: 0 })
            }
            fn end(&mut self, _s: Session) -> Result<()> {
                Ok(())
            }
            fn identify(&mut self, s: &Session) -> Result<ChipId> {
                Ok(ChipId { raw: s.device.chip_id, bytes: s.device.chip_id_bytes })
            }
            fn reset(&mut self) -> Result<()> {
                Ok(())
            }
            fn memory(&mut self) -> Option<&mut dyn MemoryOps> {
                Some(self)
            }
        }
        impl MemoryOps for Fake {
            fn read_block(&mut self, _s: &Session, req: &BlockReq) -> Result<Vec<u8>> {
                let start = req.address as usize;
                Ok(self.mem[start..start + req.len as usize].to_vec())
            }
            fn write_block(&mut self, _s: &Session, req: &BlockReq, data: &[u8]) -> Result<()> {
                let start = req.address as usize;
                self.mem[start..start + data.len()].copy_from_slice(data);
                Ok(())
            }
            fn erase(&mut self, _s: &Session, _k: EraseKind) -> Result<()> {
                self.mem.fill(0xff);
                Ok(())
            }
            fn blank_check(&mut self, _s: &Session, _r: Region) -> Result<bool> {
                Ok(self.mem.iter().all(|&b| b == 0xff))
            }
        }

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

        let dev = Device {
            name: "TEST@DIP8".into(),
            protocol_id: 1,
            code_size: 16,
            data_size: 0,
            page_size: 8,
            chip_id: 0x1234,
            chip_id_bytes: 2,
            package: Package { pin_count: 8, name: "DIP8".into() },
            algorithm: None,
            fw_target: FwVersion(0x00_01_11),
            ..Device::default()
        };
        let mut prog: Box<dyn Programmer> = Box::new(Fake {
            info: ProgrammerInfo {
                model: "FAKE".into(),
                firmware: FwVersion(0x00_01_11),
                serial: "0".into(),
                link: LinkSpeed::High,
                voltage: 5.0,
            },
            mem: (0u8..16).collect(),
        });

        let out = Shared::default();
        let mut rep =
            JsonReporter::with_writers(Box::new(out.clone()), Box::new(Shared::default()));
        let link = prog.info().link;
        let verified = {
            let mut txn = Txn::begin(&mut *prog, &dev).unwrap();
            let (p, s) = txn.parts();
            pincheck(p, s, false, &mut rep).unwrap(); // no PinTest cap -> pass
            check_chip_id(p, s, &dev, false, &mut rep).unwrap();
            ops::read_verified(p, s, Region::code(&dev), &mut rep).unwrap()
        };
        rep.finish(&verified.outcome(&dev.name, link));

        let stdout = String::from_utf8(out.0.lock().unwrap().clone()).unwrap();
        let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
        assert_eq!(v["op"], "read");
        assert_eq!(v["ok"], true);
        assert_eq!(v["dev"], "TEST@DIP8");
        assert_eq!(v["bytes"], 16);
        assert_eq!(v["stable"], true);
        assert_eq!(v["reads"], 2);
    }
}
