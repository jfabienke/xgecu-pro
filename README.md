# xgecu-pro

**A Rust redesign and reimplementation of [minipro](https://gitlab.com/nmatt0/minipro/-/tree/t76-improvements) for XGecu USB chip programmers — plus the reverse-engineering notes behind it.**

The centerpiece is [`minipro-rs/`](minipro-rs/): a Rust CLI that drives the
**T76**, **T56**, and **T48** through one trait-based driver layer. No libusb, no
zlib, no XML step — it reads XGecu's `InfoICT76.dll` chip database directly and
ships human / JSON / TUI output modes.

```
minipro info                          # identify the programmer
minipro detect                        # read the seated chip's electronic id
minipro read M27C256B@DIP28 rom.bin   # dump it, verified
```

---

## Status — read this before you write to a chip

This is a **young project handling real hardware**. Honest state of play:

| Path | Status |
|---|---|
| **T76 reads** | ✅ **Hardware-verified** — byte-identical across repeated reads on two chip classes (AT27C256R 32 KiB DIP-28, MX27C2000 256 KiB DIP-32) |
| **T76 write** (parallel EPROM) | ✅ **Hardware-verified** — programs and verifies on an MX27C2000: exactly the intended bits changed, nothing else moved |
| **T76 erase / NAND / eMMC / firmware update** | ⚠️ Implemented, **never exercised on a device**. `write --dry-run` checks the whole setup without programming |
| **T76 write** (every other chip class) | ⚠️ Only the parallel-EPROM path has touched silicon |
| **T56 / T48 (all operations)** | ⚠️ Complete drivers, **never run against real silicon** — no T48/T56 hardware here |
| **T76 pin-contact check** | ❌ **Removed** — it measured nothing *and corrupted every read*; the T76 no longer advertises it |
| **Erasing a one-time-programmable part** | ❌ Refused with a reason — the database's erase flag is honoured, as the C tool does |
| **`@ISP_VGA` parts** (monitor EDID, MStar scaler flash) | ❌ Not implemented — refused with a reason; the C tool rejects these too |
| **TL866II+ / TL866A/CS** | ❌ Not implemented |

Every driver is pinned by **byte-exact golden-packet tests** (203 tests, hardware-free,
plus 5 `#[ignore]`d ones that need a real device), so the wire output is known-correct
against captures.

A green suite is not the same as a working operation, and this project has the
scars to prove it: the write path passed every test while programming *nothing*,
because no test could tell "wrote correctly" from "wrote nothing" — only a chip
could. **Reading is non-destructive; writing and erasing are not.** Treat the
untested rows above accordingly, and see [Contributing](#contributing) if you can
help close the gap.

## Quick start

```sh
cd minipro-rs
cargo build --release          # no libusb, no pkg-config
./target/release/minipro info
```

Requires Rust 1.85+. The binary is pure Rust and MIT; the only C it links is
your platform's own TLS library, used to download the chip database over HTTPS
(Security.framework on macOS, SChannel on Windows, OpenSSL on Linux).
`--no-default-features` drops that too, for a fully static MIT-only build
without the automatic database. On macOS with a T76, **use a USB-2.0 cable** — the
SuperSpeed bulk path fails on Apple Silicon and the tool will tell you so
([details](docs/ch569-usb3-notes.md)).

### The chip database

Chip parameters and the T76's per-chip FPGA bitstreams come from XGecu's own
files. **You don't have to fetch them yourself** — on first use the tool
downloads XGecu's installer archive from the
[community mirror](https://github.com/Kreeblah/XGecu_Software), unpacks just
the database out of it, and caches the result (~75 MB).

```sh
minipro search 27C256          # just works; sets itself up once
```

Unpacking needs a RAR-capable tool **already on your system** — no RAR decoder
is bundled, which is what keeps this binary MIT. Probed in order:

| Tool | Availability |
|---|---|
| `bsdtar` | **ships with macOS**; `apt install libarchive-tools` on Debian/Ubuntu |
| `unar` | `brew install unar`, `apt install unar` |
| `unrar` | RARLAB's own extractor |

If none is present the tool says so and tells you how to install one, or how to
extract the archive yourself and point at the result.

> ⚠️ **7-Zip cannot be used** — both `7z` and `7zz` *list* the archive fine and
> then write **zero-byte files**, which looks like success. That's why it isn't
> in the list.

Resolution order, explicit first:

| Source | When |
|---|---|
| `--db <dir\|archive>` / `MINIPRO_DB_DIR` | an explicit path wins, and a failure there is fatal rather than silently downloading |
| `--db-url <mirror>` / `MINIPRO_DB_URL` | a mirror serving already-extracted files (downloads only the DLL plus bitstreams on demand) |
| **vendor archive** (default) | fetched once, unpacked, cached; override with `MINIPRO_VENDOR_URL` |
| local databases | anything already on disk, so a machine set up once keeps working offline |

Failures say which step was tried, why it failed — offline vs. the mirror moved
vs. a corrupt download vs. no extractor — and what to do instead. `minipro info`
needs no database at all and works offline.

The 63 MB download is the vendor's packaging, not a design choice: the outer
`.rar` holds the installer as a *single* compressed entry, so there is no
smaller subset to request. Use `--db-url` against an extracted mirror if you
want byte-minimal fetches.

## What it does

`read` · `write` · `erase` · `info` · `search` · `detect` · `logic` ·
`autodetect` · `tui`

Across the three drivers: memory read/write/erase/blank-check/identify, MCU
fuses, JEDEC/PLD rows, write-protect, calibration, logic-IC testing, SPI
autodetect — plus eMMC/NAND and firmware update on the T76.
Full capability matrix in the [`minipro-rs` README](minipro-rs/README.md).

Design notes worth knowing: reads are **verified by default** (re-read stability
plus crc32/sha256 in the outcome), and the type system encodes the T76's nastiest
hardware quirk — an undrained USB response wedges the device until replug, so the
must-drain guard makes forgetting it a compile error.

Deadlines are **per command, not per endpoint**: an ordinary reply gets seconds,
and only the commands that genuinely block on the chip (a full-chip erase) get
minutes. Measured on a live T76, no ordinary reply took longer than 200 ms, so a
device that stops answering surfaces as a typed error rather than an apparent
freeze.

`write --dry-run` runs every step up to the moment of programming and stops:
image and size validation, the FPGA bitstream upload, the per-chip-class
`BEGIN_TRANS`, and the chip-id match. It energizes the socket
exactly as a read does — `begin()` takes only the device, not the operation — so
it is safe on parts a bad write would destroy, like an OTP EPROM.

## Output modes

Every command renders through one of three reporters over the same event stream —
the core never prints, so no mode is a scraped afterthought.

| Mode | Selected by | For |
|---|---|---|
| **Human** | default | Reading at a terminal |
| **JSON** | `--json` or `MINIPRO_OUTPUT=json` | Scripts, CI, agents |
| **TUI** | `minipro tui` | Interactive exploration |

Precedence: the `tui` subcommand wins, then `--json` / `MINIPRO_OUTPUT`, else human.

**Human** — tables, colour, progress bars:

```
$ minipro info
┌─────────────┬──────────────────────┐
│ programmer  ┆ value                │
╞═════════════╪══════════════════════╡
│ model       ┆ T76                  │
│ firmware    ┆ 00.1.07              │
│ link        ┆ High Speed (USB 2.0) │
│ vcc         ┆ 5.1 V                │
└─────────────┴──────────────────────┘
```

**JSON** — NDJSON, with the **final outcome on stdout** and progress events on
stderr, so `minipro --json read … > outcome.json` captures exactly the result:

```console
$ minipro --json info
{"op":"info","ok":true,"model":"T76","fw":"00.1.07","fw_expected":"00.1.07","serial":"…","mfg_date":"…","device_code":"…","link":"hs","vcc":5.149}
```

Failures are structured too, and carry a **stable machine code** — `code` is a
contract (never localized, safe to branch on); `msg` is human-facing and may change:

```console
$ minipro --json read M27C256B@DIP28 rom.bin
{"code":"format","msg":"no chip database: pass --db <dir>, set MINIPRO_DB_DIR, or --db-url <mirror>","ok":false,"op":"read"}
```

Codes are `usb`, `protocol`, `chip_id_mismatch`, `bad_contact`, `overcurrent`,
`firmware_mismatch`, `verify_failed`, `no_device_response`, `unsupported`,
`format`, `io`. Where a remediation exists, the outcome carries a `hint` — e.g. a
`usb` failure on macOS suggests the USB-2.0 cable, and `no_device_response`
(autodetect read an idle bus rather than a chip) suggests what to check.

**TUI** — a [ratatui](https://ratatui.rs) interface with a searchable chip-DB
browser, a 40-pin ZIF contact map, an operation progress gauge, a hex view, and a
log pane. The contact map renders whatever the pin check reports; with the T76's
check removed it stays a layout rather than a measurement (see the status table).

## Repo layout

```
minipro-rs/     The Rust workspace — core / proto (drivers) / usb / db / cli
docs/           Reverse-engineering notes, protocol analysis, design docs
docs/hardware/  FPGA + MCU pinouts and schematic (Anlogic EG4X20 + WCH CH569W)
dumps/          Verified ROM dumps read with this tooling (Adaptec AHA-1542CP)
submission/     Those dumps written up for preservation archives
hardware/       USB identity notes for the T76
```

## Documentation

Reverse-engineering and analysis, useful independently of the Rust code:

| Document | What's in it |
|---|---|
| [`t76-protocol.md`](docs/t76-protocol.md) | The T76 USB wire protocol — opcodes, packet layouts, operation sequences |
| [`infoict76-dll-format.md`](docs/infoict76-dll-format.md) | `InfoICT76.dll` on-disk format: chip-descriptor offsets and field transforms |
| [`firmware-updater.md`](docs/firmware-updater.md) | The `updateT76.dat` container and the bootloader flashing sequence |
| [`ch569-usb3-notes.md`](docs/ch569-usb3-notes.md) | The CH569 USB 3.0 stack and the macOS SuperSpeed failure |
| [`open-source-status.md`](docs/open-source-status.md) | Landscape of open-source T76 support |
| [`minipro-codebase-report.md`](docs/minipro-codebase-report.md) | Review of the upstream C implementation |
| [`minipro-setup.md`](docs/minipro-setup.md) | Building the C fork locally — the reference this project is checked against |
| [`rust-redesign.md`](docs/rust-redesign.md) · [`rust-trait-model.md`](docs/rust-trait-model.md) · [`rust-roadmap.md`](docs/rust-roadmap.md) | The design this project was built from, and what's next |
| [`design-decisions.md`](docs/design-decisions.md) | The decisions in retrospect — what was chosen over the C's way, why, and how each fared against real silicon |
| [`minipro-vs-rust.md`](docs/minipro-vs-rust.md) | Honest comparison with the C tool |
| [`multi-programmer.md`](docs/multi-programmer.md) | Driving a bank of programmers in parallel — what's ready, and the reset bug to fix first |

## Contributing

The most valuable contribution is **hardware nobody here has**:

- **🔌 Run it against a real T48 or T56.** The drivers are complete and
  golden-tested but have never touched silicon. `minipro info`, `detect`, and a
  read of a known chip — reporting what you see — turns "reference-only" into
  "proven."
- **🔌 Erase, and write on a rewritable part.** One salvaged 25-series SPI flash
  (dead motherboard, router, TV mainboard) closes three gaps at once: the erase
  path, write-buffer chunking beyond the parallel-EPROM case, and the long
  erase deadline. On flash a verified dump makes the whole loop recoverable —
  dump, erase, write, verify, compare.
- **Implement the TL866II+ / TL866A/CS drivers.** The TL866II+ is close to the
  existing T48 and reuses most of the shared wire layer.

There are also hardware tests that only run when you ask, and they need nothing
in the socket — no chip, no adapter:

```sh
# Run one at a time: they share the single attached device, and back-to-back
# runs are stateful enough to trip each other up.
cargo test -p minipro-usb -- --ignored --nocapture --test-threads=1 live_command
```

Bug reports are most useful with the programmer model, `minipro info` output,
the exact command, and a `--json` line. See the
[roadmap](docs/rust-roadmap.md) for scoped work items.

## Credits

This project stands on **[Matt Brown (nmatt0)](https://github.com/nmatt0)**'s
reverse engineering in the
[`t76-improvements` minipro fork](https://gitlab.com/nmatt0/minipro/-/tree/t76-improvements) —
the USB protocols, the T76's FPGA/bitstream lifecycle, the firmware-update
transport, and the DLL field transforms. None of this would exist without that
work.

Also: **[radiomanV](https://github.com/radiomanV/Xgecu_T76)** for FPGA-side
tooling and hardware documentation, and
**[Kreeblah](https://github.com/Kreeblah/XGecu_Software)** for keeping the
vendor-software mirror current.

See [`NOTICE`](NOTICE) for full attribution.

## License

Original work in this repository is **[MIT](LICENSE)**.

The MIT grant covers this project's own code and documentation only.

The default binary bundles no third-party code beyond Rust crates: the chip
database is unpacked by a tool you already have, never by a bundled RAR
decoder. On Linux the TLS library it links is the system OpenSSL;
`--no-default-features` removes even that.

Proprietary third-party material — XGecu's Xgpro application and
`InfoICT76.dll`, vendor datasheets, device-support lists, firmware images, and
`.alg` bitstreams — is **not redistributed here** and is not covered by the MIT
grant. The tool downloads XGecu's installer from a community mirror at runtime
and reads it locally; it never republishes it.
