// SPDX-FileCopyrightText: 2026 John Fabienke
// SPDX-License-Identifier: MIT

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

use clap::{CommandFactory, Parser, Subcommand, ValueEnum};
use minipro_core::device::{EraseKind, Image, Region};
use minipro_core::error::{Error, Result};
use minipro_core::format::Format;
use minipro_core::ops;
use minipro_core::programmer::Txn;
use minipro_core::report::{Event, Outcome, Reporter, Warning};
use minipro_core::Programmer;
use minipro_db::{ChipDb, DllDb, XmlDb};
use reporters::{HumanReporter, JsonReporter};

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

    /// Provision the chip database from a mirror of already-extracted files,
    /// opt-in. Caches the vendor files as-is — InfoICT76.dll beside an algoT76/
    /// directory — under the --db cache dir (or a default cache), so the result
    /// is directly reusable as `--db <cache>/mirror`. The first run each day
    /// checks the mirror for a new version. Mirror serves the extracted files:
    /// `<url>/InfoICT76.dll`, `<url>/algoT76/<algo>.alg`.
    #[arg(long, global = true, env = "MINIPRO_DB_URL", value_name = "URL")]
    db_url: Option<String>,

    /// Select chip (legacy flag; pairs with -r/-w)
    #[arg(short = 'p', value_name = "CHIP", help_heading = "Legacy flags")]
    legacy_chip: Option<String>,

    /// Read chip into FILE (legacy alias for `minipro read`)
    #[arg(
        short = 'r',
        value_name = "FILE",
        help_heading = "Legacy flags",
        requires = "legacy_chip"
    )]
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

/// Image file format selector. `Auto` picks by extension (`.hex`→ihex,
/// `.s19/.srec/…`→srec, else raw); on write it also sniffs the content.
#[derive(ValueEnum, Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Fmt {
    #[default]
    Auto,
    Raw,
    Ihex,
    Srec,
}

impl Fmt {
    /// The concrete [`Format`] for reading `file` back (output side).
    fn for_output(self, file: &Path) -> Format {
        match self {
            Fmt::Auto => Format::from_path(file),
            Fmt::Raw => Format::Raw,
            Fmt::Ihex => Format::IHex,
            Fmt::Srec => Format::SRec,
        }
    }
    /// The concrete [`Format`] for parsing `file` (input side): `Auto` uses the
    /// extension, falling back to content sniffing for a mislabeled file.
    fn for_input(self, file: &Path, bytes: &[u8]) -> Format {
        match self {
            Fmt::Auto => match Format::from_path(file) {
                Format::Raw => Format::detect(bytes),
                known => known,
            },
            other => other.for_output(file),
        }
    }
}

#[derive(Subcommand, Debug, Clone)]
enum Command {
    /// Read a chip into FILE, with re-read verification (crc32/sha256/stable)
    Read {
        /// Chip name, e.g. "M27C256B@DIP28"
        chip: String,
        /// Output file for the dump
        file: PathBuf,
        /// Output format (default: by file extension)
        #[arg(long, value_enum, default_value_t = Fmt::Auto)]
        format: Fmt,
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
        /// Input format (default: by extension, else sniffed)
        #[arg(long, value_enum, default_value_t = Fmt::Auto)]
        format: Fmt,
        /// Proceed despite a chip-id mismatch
        #[arg(long)]
        force: bool,
        /// Skip the pin-contact check
        #[arg(long)]
        skip_pincheck: bool,
        /// Run every step up to the point of programming, then stop without
        /// modifying the chip. Energizes the socket exactly as a read does, so
        /// it is safe on one-time-programmable parts.
        #[arg(long)]
        dry_run: bool,
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
    /// Functionally test a logic IC (74xx/40xx) against its vector table
    Logic {
        /// Logic-IC name, e.g. "7400@DIP14" (from the logic database)
        chip: String,
    },
    /// Autodetect a seated SPI 25-series flash by its JEDEC id
    Autodetect {
        /// Probe a 16-pin (SOIC16) package instead of the 8-pin default
        #[arg(long)]
        wide: bool,
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
            Command::Logic { .. } => "logic",
            Command::Autodetect { .. } => "autodetect",
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
                format: Fmt::Auto,
                force: false,
                skip_pincheck: false,
            })),
            (Some(chip), None, Some(file)) => Ok(Some(Command::Write {
                chip: chip.clone(),
                file: file.clone(),
                format: Fmt::Auto,
                force: false,
                skip_pincheck: false,
                // The legacy -p/-w form predates the flag and always commits.
                dry_run: false,
            })),
            (Some(_), None, None) => Err(Error::Format(
                "-p needs -r FILE (read) or -w FILE (write)".into(),
            )),
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

/// Install the tracing subscriber (events to stderr, so JSON on stdout stays
/// clean). `RUST_LOG` has full control; `MINIPRO_TRACE=1` is kept as the
/// established alias for a full wire trace; default is warnings only.
fn init_tracing() {
    use tracing_subscriber::filter::{LevelFilter, Targets};
    use tracing_subscriber::layer::SubscriberExt as _;
    use tracing_subscriber::util::SubscriberInitExt as _;
    let spec = std::env::var("RUST_LOG").ok().unwrap_or_else(|| {
        if std::env::var_os("MINIPRO_TRACE").is_some() {
            "minipro_proto=trace,minipro_usb=trace".into()
        } else {
            "warn".into()
        }
    });
    let filter: Targets = spec
        .parse()
        .unwrap_or_else(|_| Targets::new().with_default(LevelFilter::WARN));
    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
        .with(filter)
        .init();
}

fn main() -> ExitCode {
    init_tracing();
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
        Command::Read {
            chip,
            file,
            format,
            force,
            skip_pincheck,
        } => run_read(
            db_dir,
            chip,
            file,
            *format,
            *force,
            *skip_pincheck,
            &mut *reporter_for(mode),
        ),
        Command::Write {
            chip,
            file,
            format,
            force,
            skip_pincheck,
            dry_run,
        } => run_write(
            db_dir,
            chip,
            file,
            *format,
            *force,
            *skip_pincheck,
            *dry_run,
            &mut *reporter_for(mode),
        ),
        Command::Erase { chip } => run_erase(db_dir, chip, &mut *reporter_for(mode)),
        Command::Info => run_info(db_dir, &mut *reporter_for(mode)),
        Command::Detect { like } => run_detect(db_dir, like, mode),
        Command::Logic { chip } => run_logic(db_dir, chip, mode),
        Command::Autodetect { wide } => run_autodetect(db_dir, *wide, mode),
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

/// Open the USB transport and detect the attached programmer. Probes all known
/// USB ids (the T76 and the shared TL866II+/T48/T56 id), then `detect()` reads
/// the system-info byte to bind the right driver.
/// With nothing plugged in this returns the transport's "no device" error.
pub(crate) fn open_programmer() -> Result<Box<dyn Programmer>> {
    let tx = minipro_usb::UsbTransport::open_any()?;
    tx.check_link()?; // macOS + SuperSpeed diagnosis, not a bare I/O error
    minipro_proto::detect(Box::new(tx))
}

/// Load the chip database. Boxed so callers stay backend-agnostic once a
/// compiled/embedded backend implements `ChipDb` too.
/// Where the downloaded chip database lives.
///
/// `XDG_CACHE_HOME` wins when set — it is the documented override and works
/// the same on every platform, which matters for scripting and tests. Failing
/// that, the platform's own convention via `directories`: `~/Library/Caches`
/// on macOS, `%LOCALAPPDATA%` on Windows, `~/.cache` on Linux. Hand-rolling
/// that lookup got macOS wrong.
fn default_cache_dir() -> PathBuf {
    if let Ok(x) = std::env::var("XDG_CACHE_HOME") {
        if !x.is_empty() {
            return PathBuf::from(x).join("minipro");
        }
    }
    if let Some(dirs) = directories::ProjectDirs::from("", "", "minipro") {
        return dirs.cache_dir().to_path_buf();
    }
    std::env::temp_dir().join("minipro-cache")
}

/// Open a vendor archive by unpacking it (once) into the cache with whatever
/// RAR tool the system has, then reading the result.
fn load_archive_db(path: &Path) -> Result<Box<dyn ChipDb>> {
    let dest = default_cache_dir().join("xgpro");
    let dir = minipro_db::extract::unpack_vendor_archive(path, &dest)?;
    Ok(Box::new(DllDb::load(&dir)?))
}

#[cfg(feature = "net")]
fn load_mirror_db(url: &str, cache: &Path) -> Result<Box<dyn ChipDb>> {
    Ok(Box::new(minipro_db::HttpDb::open(url, cache, None)?))
}

#[cfg(not(feature = "net"))]
fn load_mirror_db(_url: &str, _cache: &Path) -> Result<Box<dyn ChipDb>> {
    Err(Error::Unsupported(
        "this build has no mirror support; rebuild with `--features net`, or pass --db <dir>",
    ))
}

/// Open a `--db` path, which may be a vendor archive or an extracted directory.
fn load_local_db(path: &Path) -> Result<Box<dyn ChipDb>> {
    if minipro_db::extract::is_vendor_archive(path) {
        return load_archive_db(path);
    }
    if !path.is_dir() {
        return Err(Error::Format(format!(
            "{} is neither a directory nor a vendor .rar/.exe",
            path.display()
        )));
    }
    if path.join("InfoICT76.dll").is_file() {
        Ok(Box::new(DllDb::load(path)?))
    } else if path.join("infoic.xml").is_file() {
        Ok(Box::new(XmlDb::load(path)?))
    } else {
        Err(Error::Format(format!(
            "{} holds no chip database (expected InfoICT76.dll or infoic.xml)",
            path.display()
        )))
    }
}

/// Places a database may already be sitting, checked before giving up. Keeps a
/// machine that has been set up once working even when the network is gone.
fn local_candidates() -> Vec<PathBuf> {
    let mut v = Vec::new();
    let cache = default_cache_dir();
    v.push(cache.join("xgpro")); // unpacked by the default source
    v.push(cache.join("mirror")); // provisioned from a --db-url mirror
    v.push(cache.join("xgpro_vendor.rar")); // a previous default-source download
    if let Ok(home) = std::env::var("HOME") {
        v.push(PathBuf::from(&home).join(".local/share/minipro"));
        v.push(PathBuf::from(&home).join("Xgpro_T76"));
    }
    v.push(PathBuf::from("Xgpro_T76"));
    v
}

/// The zero-setup default: XGecu's own installer archive from the community
/// mirror, cached and read in place (nothing proprietary is unpacked to disk).
#[cfg(feature = "net")]
fn load_default_source(rep: &mut dyn Reporter) -> std::result::Result<Box<dyn ChipDb>, String> {
    use minipro_db::vendor;
    let cache = default_cache_dir();
    // Overridable so a user behind a restricted network (or a newer vendor
    // release) can retarget the default source without patching the binary.
    let url_owned = std::env::var("MINIPRO_VENDOR_URL").ok();
    let url = url_owned
        .as_deref()
        .filter(|u| !u.is_empty())
        .unwrap_or(vendor::DEFAULT_VENDOR_ARCHIVE);
    if vendor::cached_archive(&cache).is_none() {
        rep.event(&Event::Note(
            format!(
                "fetching the chip database once from {url} (~63 MB, cached at {})",
                cache.display()
            )
            .into(),
        ));
    }
    match vendor::open(&cache, url) {
        Ok(Ok(db)) => Ok(Box::new(db)),
        // Downloaded fine, but could not be unpacked (usually: no extractor).
        // Strip the category prefix: this message is already self-describing.
        Ok(Err(e)) => Err(e
            .to_string()
            .strip_prefix("format: ")
            .unwrap_or(&e.to_string())
            .to_string()),
        Err(e) => Err(e.explain()),
    }
}

#[cfg(not(feature = "net"))]
fn load_default_source(_rep: &mut dyn Reporter) -> std::result::Result<Box<dyn ChipDb>, String> {
    Err("this build has no default database source (needs the `net` feature)".into())
}

/// Resolve the chip database.
///
/// Explicit wins, then the zero-setup default, then anything already on disk:
///
/// 1. `--db <dir|archive>` / `MINIPRO_DB_DIR` — an explicit path is obeyed, and
///    a failure there is fatal rather than silently papered over by a download.
/// 2. `--db-url <mirror>` / `MINIPRO_DB_URL` — an explicit mirror, same rule.
/// 3. The default vendor archive (fetched once, cached).
/// 4. Databases already present locally, so a machine set up earlier keeps
///    working offline.
///
/// If every step fails, the error lists what was tried and how to fix it —
/// the fallbacks are useless if the user cannot see them.
fn load_db(dir: Option<&Path>, rep: &mut dyn Reporter) -> Result<Box<dyn ChipDb>> {
    // 1 + 2: explicit sources are authoritative; do not fall back past them.
    if let Some(path) = dir {
        return load_local_db(path);
    }
    if let Ok(url) = std::env::var("MINIPRO_DB_URL") {
        if !url.is_empty() {
            return load_mirror_db(&url, &default_cache_dir());
        }
    }

    // 3: the default source.
    let default_err = match load_default_source(rep) {
        Ok(db) => return Ok(db),
        Err(msg) => msg,
    };

    // 4: anything already on disk.
    for cand in local_candidates() {
        if cand.exists() {
            if let Ok(db) = load_local_db(&cand) {
                rep.event(&Event::Note(
                    format!(
                        "using the local chip database at {} (the default source was unavailable)",
                        cand.display()
                    )
                    .into(),
                ));
                return Ok(db);
            }
        }
    }

    Err(Error::Format(format!(
        "no chip database available.\n\n{default_err}\n\nAlso looked for a local database in:\n{}",
        local_candidates()
            .iter()
            .map(|p| format!("  {}", p.display()))
            .collect::<Vec<_>>()
            .join("\n")
    )))
}

/// Look up a chip and attach its FPGA bitstream from `algorithm.xml` (the DB
/// stores chip parameters and bitstreams separately; `begin` needs both).
fn lookup_device(db: &dyn ChipDb, chip: &str) -> Result<minipro_core::device::Device> {
    use minipro_core::device::chip_type;
    let mut dev = db
        .get(chip)
        .cloned()
        .ok_or(Error::Unsupported("unknown chip (try `minipro search`)"))?;
    // The catalog lists 30 `@ISP_VGA` parts — monitor EDID EEPROMs and MStar
    // scaler flash — which are programmed down a display cable rather than
    // through the ZIF socket. Without that rejection the algorithm resolves and
    // the socket path runs anyway, failing later with something unrelated to
    // the real reason. Caught here so every operation reports it the same way;
    // `search` does not come through here, so they stay discoverable.
    if dev.chip_type == chip_type::VGA {
        return Err(Error::Unsupported(
            "programming over a display's VGA/DDC connection is not implemented \
             (@ISP_VGA parts: monitor EDID and MStar scaler flash)",
        ));
    }
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

/// Contact check — **advisory**: reports poor seating and continues.
///
/// It used to abort the operation, contradicting the design (see
/// `docs/rust-redesign.md`, "Pin-detect is advisory, not fatal"). Two things
/// argue for warning only. The decode is a hint, not a fact: the reply's
/// payload semantics are still unconfirmed, and until they were fixed the check
/// misread the opcode echo and failed on pins 2-6 every run. And even a true
/// bad-contact report is not decisive — an oxidized vintage part can read
/// perfectly, which is exactly how the AHA-1542CP dumps in `dumps/` were taken.
///
/// A read is non-destructive and verified by re-reading, so the honest split is
/// to surface the warning and let the result speak. Programmers without
/// pin-test support pass silently.
fn pincheck(
    prog: &mut dyn Programmer,
    s: &minipro_core::Session,
    skip: bool,
    rep: &mut dyn Reporter,
) -> Result<()> {
    if skip {
        return Ok(());
    }
    let Some(pins) = prog.pins() else {
        return Ok(());
    };
    // A failure to *run* the check is a protocol error and still propagates;
    // only its verdict is advisory.
    let open = pins.contact_check(s)?;
    if !open.is_empty() {
        rep.event(&Event::Warn(Warning::BadContact(open)));
    }
    Ok(())
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
        rep.event(&Event::Warn(Warning::ChipIdMismatch {
            expected: dev.chip_id,
            got: id.raw,
        }));
        Ok(())
    } else {
        Err(Error::ChipIdMismatch {
            expected: dev.chip_id,
            got: id.raw,
            alias: None,
        })
    }
}

#[allow(clippy::too_many_arguments)]
fn run_read(
    db_dir: Option<&Path>,
    chip: &str,
    file: &Path,
    format: Fmt,
    force: bool,
    skip_pincheck: bool,
    rep: &mut dyn Reporter,
) -> Result<()> {
    let db = load_db(db_dir, rep)?;
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

    let out = format.for_output(file).emit(&verified.image);
    std::fs::write(file, &out)?;
    rep.finish(&verified.outcome(&dev.name, link));
    Ok(())
}

/// Parse `file` in the selected format and fit it to the chip: pad short images
/// to `code_size` with the chip's erased byte (`blank`, so read-back verify of
/// the tail matches the real erased state), and reject one larger than the chip.
fn load_image(file: &Path, format: Fmt, code_size: u64, blank: u8) -> Result<Image> {
    let raw = std::fs::read(file)?;
    let mut image = format.for_input(file, &raw).parse(&raw, blank)?;
    let need = code_size as usize;
    match image.bytes.len().cmp(&need) {
        std::cmp::Ordering::Greater => Err(Error::Format(format!(
            "image is {} bytes but the chip holds {} — too large",
            image.bytes.len(),
            need
        ))),
        std::cmp::Ordering::Less => {
            image.bytes.resize(need, blank); // pad the tail with the erased byte
            Ok(image)
        }
        std::cmp::Ordering::Equal => Ok(image),
    }
}

#[allow(clippy::too_many_arguments)]
fn run_write(
    db_dir: Option<&Path>,
    chip: &str,
    file: &Path,
    format: Fmt,
    force: bool,
    skip_pincheck: bool,
    dry_run: bool,
    rep: &mut dyn Reporter,
) -> Result<()> {
    let db = load_db(db_dir, rep)?;
    let dev = lookup_device(&*db, chip)?;
    let image = load_image(file, format, dev.code_size, dev.blank_value)?;
    let mut prog = open_programmer()?;
    warn_firmware(&*prog, &*db, rep);

    {
        let mut txn = Txn::begin(&mut *prog, &dev)?;
        let (p, s) = txn.parts();
        pincheck(p, s, skip_pincheck, rep)?;
        check_chip_id(p, s, &dev, force, rep)?;
        // Everything above is what a read does too — `Txn::begin` takes only the
        // device, so the socket is energized identically either way. The single
        // destructive step is below, which is what `--dry-run` skips: it makes
        // the setup verifiable on parts that cannot survive a bad write (an OTP
        // EPROM, or anything whose contents matter).
        if dry_run {
            rep.finish(&Outcome::Ok {
                op: "write-dry-run",
            });
            return Ok(());
        }
        ops::write_region(p, s, Region::code(&dev), &image, rep)?;
    }

    rep.finish(&Outcome::Ok { op: "write" });
    Ok(())
}

fn run_erase(db_dir: Option<&Path>, chip: &str, rep: &mut dyn Reporter) -> Result<()> {
    let db = load_db(db_dir, rep)?;
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
        Some(dir) => load_db(Some(dir), rep)?.firmware_target().to_string(),
        None => firmware.clone(),
    };
    rep.finish(&Outcome::Info {
        model: info.model.clone(),
        firmware,
        firmware_expected,
        serial: info.serial.clone(),
        mfg_date: info.mfg_date.clone(),
        device_code: info.device_code.clone(),
        link: info.link,
        vcc: info.voltage,
    });
    Ok(())
}

/// Whether a 24-bit autodetect result is a chip answering at all.
///
/// A floating SPI bus settles to all-zeros or all-ones depending on which way
/// the bitstream biases it, and JEDEC assigns neither `0x00` nor `0xff` as a
/// manufacturer id. Reporting these as a successful detection of an "unknown
/// vendor" hides the far likelier truth: nothing drove the bus.
fn responded(id: u32) -> bool {
    id != 0x000000 && id != 0xff_ffff
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
    let mut note = reporter_for(mode);
    let db = load_db(db_dir, note.as_mut())?;
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
            anstream::println!(
                "Electronic id: 0x{:0width$X}  ({} bytes) — {}",
                id.raw,
                id.bytes,
                mfr
            );
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

/// A bitstream loader over a `ChipDb`: fetch a utility algorithm's decoded
/// bytes by name (the FPGA logic-test / autodetect ops call this per pass).
fn bitstream_loader(db: &dyn ChipDb) -> impl FnMut(&str) -> Result<Vec<u8>> + '_ {
    move |name: &str| {
        db.load_algorithm_named(name)?
            .map(|a| a.bitstream)
            .ok_or(Error::Unsupported(
                "utility bitstream not found in the database",
            ))
    }
}

/// Functionally test a logic IC. Logic devices (from `logicic.xml`) carry their
/// vector table; the driver uploads the FPGA logic bitstreams itself (T56/T76)
/// or bit-bangs the vectors (T48), so there is no `begin`/algorithm here.
fn run_logic(db_dir: Option<&Path>, chip: &str, mode: Mode) -> Result<()> {
    use minipro_core::device::chip_type;
    let mut note = reporter_for(mode);
    let db = load_db(db_dir, note.as_mut())?;
    let dev = lookup_device(&*db, chip)?;
    if dev.chip_type != chip_type::LOGIC || dev.vectors.is_empty() {
        return Err(Error::Unsupported(
            "not a logic IC with a vector table (is logicic.xml present?)",
        ));
    }
    let mut prog = open_programmer()?;
    let session = minipro_core::Session {
        device: dev.clone(),
        emmc_capacity: 0,
    };
    let pass = {
        let mut load = bitstream_loader(&*db);
        let logic = prog
            .logic()
            .ok_or(Error::Unsupported("this programmer has no logic test"))?;
        logic.run(&session, &mut load)?
    };
    prog.end(session)?; // de-energize the socket (end the transaction)

    match mode {
        Mode::Json => {
            println!(
                "{{\"op\":\"logic\",\"ok\":true,\"device\":{:?},\"pass\":{pass}}}",
                dev.name
            )
        }
        _ => anstream::println!(
            "Logic test {}: {}",
            dev.name,
            if pass { "PASS" } else { "FAIL" }
        ),
    }
    Ok(())
}

/// Autodetect a seated SPI 25-series flash. No device is known up front — the
/// driver uploads the SPI25F probe bitstream and returns the JEDEC id.
fn run_autodetect(db_dir: Option<&Path>, wide: bool, mode: Mode) -> Result<()> {
    let mut note = reporter_for(mode);
    let db = load_db(db_dir, note.as_mut())?;
    let mut prog = open_programmer()?;
    let id = {
        let mut load = bitstream_loader(&*db);
        let ad = prog
            .autodetect()
            .ok_or(Error::Unsupported("this programmer has no SPI autodetect"))?;
        ad.spi_autodetect(wide, &mut load)?
    };
    if !responded(id) {
        return Err(Error::NoDeviceResponse { id });
    }
    let mfr = manufacturer_name((id >> 16) as u8);
    match mode {
        Mode::Json => {
            println!("{{\"op\":\"autodetect\",\"ok\":true,\"id\":\"{id:06x}\",\"manufacturer\":\"{mfr}\"}}")
        }
        _ => anstream::println!("SPI autodetect: 0x{id:06X} — {mfr}"),
    }
    Ok(())
}

fn run_search(db_dir: Option<&Path>, query: &str, limit: usize, mode: Mode) -> Result<()> {
    let mut note = reporter_for(mode);
    let db = load_db(db_dir, note.as_mut())?;
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
    use minipro_core::format::PAD;

    fn parse(args: &[&str]) -> Cli {
        Cli::try_parse_from(args).expect("args parse")
    }

    /// `--dry-run` must be opt-in. A default that skipped the write would be a
    /// silent no-op; a default that committed when asked not to would destroy a
    /// part. Both directions are worth pinning.
    #[test]
    fn dry_run_is_opt_in_and_reaches_the_command() {
        let plain = parse(&["minipro", "write", "CHIP@DIP28", "rom.bin"]);
        match plain.command {
            Some(Command::Write { dry_run, .. }) => assert!(!dry_run, "must commit by default"),
            other => panic!("expected a write command, got {other:?}"),
        }
        let dry = parse(&["minipro", "write", "CHIP@DIP28", "rom.bin", "--dry-run"]);
        match dry.command {
            Some(Command::Write { dry_run, .. }) => assert!(dry_run, "--dry-run must be honoured"),
            other => panic!("expected a write command, got {other:?}"),
        }
        // The legacy -p/-w form has no such flag and must never dry-run.
        let legacy = parse(&["minipro", "-p", "CHIP@DIP28", "-w", "rom.bin"]);
        match legacy.resolve_command().expect("resolves") {
            Some(Command::Write { dry_run, .. }) => assert!(!dry_run),
            other => panic!("expected a write command, got {other:?}"),
        }
    }

    /// `@ISP_VGA` parts are programmed down a display cable, not through the
    /// ZIF socket. Without the gate in `lookup_device` the algorithm resolves
    /// and the socket path runs anyway, so the user sees a failure unrelated to
    /// the real reason. The C tool rejects these too.
    #[test]
    fn vga_devices_are_rejected_with_a_reason() {
        use minipro_core::device::{chip_type, Algorithm, Device};
        use minipro_core::error::FwVersion;
        use minipro_db::Search;

        struct Db(Device);
        impl ChipDb for Db {
            fn get(&self, name: &str) -> Option<&Device> {
                (name == self.0.name).then_some(&self.0)
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
            fn load_algorithm(&self, _d: &Device) -> Result<Option<Algorithm>> {
                Ok(None)
            }
        }

        let vga = Db(Device {
            name: "EDID_128B@ISP_VGA".into(),
            chip_type: chip_type::VGA,
            ..Device::default()
        });
        let err = lookup_device(&vga, "EDID_128B@ISP_VGA").expect_err("must be rejected");
        assert_eq!(err.code(), "unsupported");
        let msg = err.to_string();
        assert!(msg.contains("VGA/DDC"), "must name the interface: {msg}");
        assert!(msg.contains("ISP_VGA"), "must name the parts: {msg}");

        // A socket part of the same shape must still resolve — the gate keys on
        // the chip type, not on anything incidental.
        let ok = Db(Device {
            name: "W25Q64BV@SOIC8".into(),
            chip_type: chip_type::MEMORY,
            ..Device::default()
        });
        assert!(lookup_device(&ok, "W25Q64BV@SOIC8").is_ok());
    }

    #[test]
    fn idle_bus_patterns_are_not_a_detection() {
        // The two ways a floating SPI bus reads back, seen on real hardware:
        // the narrow bitstream biases it low, the wide one high.
        assert!(!responded(0x000000), "all-zeros is an idle bus");
        assert!(!responded(0xff_ffff), "all-ones is an idle bus");
        // A real Winbond W25Q64 must still detect.
        assert!(responded(0xef4017));
        // 0x00/0xff are only meaningless as the *manufacturer* byte; a valid
        // vendor with zero device bytes is still a response.
        assert!(responded(0xef0000));
    }

    #[test]
    fn clap_definition_is_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn fmt_selection_output_and_input() {
        // Output side: extension picks the format; explicit overrides win.
        assert_eq!(Fmt::Auto.for_output(Path::new("d.hex")), Format::IHex);
        assert_eq!(Fmt::Auto.for_output(Path::new("d.bin")), Format::Raw);
        assert_eq!(Fmt::Raw.for_output(Path::new("d.hex")), Format::Raw);
        // Input side: Auto sniffs content when the extension is unknown.
        assert_eq!(
            Fmt::Auto.for_input(Path::new("d.dat"), b":00000001FF"),
            Format::IHex
        );
        assert_eq!(
            Fmt::Auto.for_input(Path::new("d.dat"), &[0, 1, 2]),
            Format::Raw
        );
    }

    #[test]
    fn load_image_fits_to_chip() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("minipro-fmt-{}.bin", std::process::id()));
        std::fs::write(&path, [0xaa, 0xbb, 0xcc, 0xdd]).unwrap();

        // Short image is padded to code_size with the given blank byte (0x00
        // here, proving the pad is the chip's erased value, not a hardcoded 0xFF).
        let img = load_image(&path, Fmt::Raw, 8, 0x00).unwrap();
        assert_eq!(img.bytes, vec![0xaa, 0xbb, 0xcc, 0xdd, 0, 0, 0, 0]);

        // Exact fit is untouched.
        assert_eq!(
            load_image(&path, Fmt::Raw, 4, PAD).unwrap().bytes,
            vec![0xaa, 0xbb, 0xcc, 0xdd]
        );

        // Larger than the chip is rejected.
        assert_eq!(
            load_image(&path, Fmt::Raw, 2, PAD).unwrap_err().code(),
            "format"
        );

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn logic_and_autodetect_parse() {
        match parse(&["minipro", "logic", "7400@DIP14"]).command.unwrap() {
            Command::Logic { chip } => assert_eq!(chip, "7400@DIP14"),
            _ => panic!("expected logic"),
        }
        match parse(&["minipro", "autodetect", "--wide"]).command.unwrap() {
            Command::Autodetect { wide } => assert!(wide),
            _ => panic!("expected autodetect"),
        }
        match parse(&["minipro", "autodetect"]).command.unwrap() {
            Command::Autodetect { wide } => assert!(!wide, "8-pin is the default"),
            _ => panic!("expected autodetect"),
        }
    }

    #[test]
    fn write_accepts_format_flag() {
        let cli = parse(&[
            "minipro",
            "write",
            "AT28C256@DIP28",
            "img.hex",
            "--format",
            "ihex",
        ]);
        match cli.command.unwrap() {
            Command::Write { format, .. } => assert_eq!(format, Fmt::Ihex),
            _ => panic!("expected write"),
        }
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
            Command::Read {
                chip,
                file,
                force,
                skip_pincheck,
                ..
            } => {
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
        assert_eq!(
            select_mode(cli.command.as_ref(), cli.json, None),
            Mode::Json
        );
    }

    /// An explicit `--db` that does not resolve must fail loudly rather than
    /// silently falling through to a download — an explicit path is a
    /// statement of intent.
    ///
    /// Deliberately does *not* exercise `load_db(None)`: that path reaches the
    /// network by design, and a unit test must stay hermetic.
    #[test]
    fn explicit_db_path_failure_is_clear() {
        let mut rep = reporter_for(Mode::Human);
        let missing = Path::new("/nonexistent/xgpro-db");
        let err = match load_db(Some(missing), rep.as_mut()) {
            Err(e) => e,
            Ok(_) => panic!("a nonexistent --db path must fail"),
        };
        assert_eq!(err.code(), "format");
        let msg = err.to_string();
        assert!(
            msg.contains("nonexistent"),
            "error should name the path: {msg}"
        );
    }

    /// The offline fallback is only useful if the user can see where it looked.
    #[test]
    fn local_candidates_are_nonempty_and_absolute_or_relative_paths() {
        let c = local_candidates();
        assert!(!c.is_empty());
        assert!(
            c.iter().any(|p| p.ends_with("xgpro_vendor.rar")),
            "a previously downloaded archive must be a candidate"
        );
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
                Ok(Session {
                    device: dev.clone(),
                    emmc_capacity: 0,
                })
            }
            fn end(&mut self, _s: Session) -> Result<()> {
                Ok(())
            }
            fn identify(&mut self, s: &Session) -> Result<ChipId> {
                Ok(ChipId {
                    raw: s.device.chip_id,
                    bytes: s.device.chip_id_bytes,
                })
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
            package: Package {
                pin_count: 8,
                name: "DIP8".into(),
            },
            algorithm: None,
            fw_target: FwVersion(0x00_01_11),
            ..Device::default()
        };
        let mut prog: Box<dyn Programmer> = Box::new(Fake {
            info: ProgrammerInfo {
                model: "FAKE".into(),
                firmware: FwVersion(0x00_01_11),
                serial: "0".into(),
                mfg_date: String::new(),
                device_code: String::new(),
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
