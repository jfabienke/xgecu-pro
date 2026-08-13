# xgecu-pro

**A Rust redesign and reimplementation of [minipro](https://gitlab.com/nmatt0/minipro/-/tree/t76-improvements) for XGecu USB chip programmers — plus the reverse-engineering notes behind it.**

The centerpiece is [`minipro-rs/`](minipro-rs/): a pure-Rust CLI that drives the
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
| **T76 reads** | ✅ **Hardware-verified** — byte-identical to known-good dumps, stable over repeated reads |
| **T76 write / erase / NAND / eMMC / firmware update** | ⚠️ Implemented, **never exercised on a device** |
| **T56 / T48 (all operations)** | ⚠️ Complete drivers, **never run against real silicon** — no T48/T56 hardware here |
| **TL866II+ / TL866A/CS** | ❌ Not implemented |

Every driver is pinned by **byte-exact golden-packet tests** (180 tests, hardware-free),
so the wire output is known-correct against captures — but a passing test suite is
not the same as a validated write. **Reading is non-destructive; writing and erasing
are not.** Treat the untested paths accordingly, and see
[Contributing](#contributing) if you can help close the gap.

## Quick start

```sh
cd minipro-rs
cargo build --release          # pure Rust; no libusb/pkg-config
./target/release/minipro info
```

Requires Rust 1.85+. On macOS with a T76, **use a USB-2.0 cable** — the
SuperSpeed bulk path fails on Apple Silicon and the tool will tell you so
([details](docs/ch569-usb3-notes.md)).

### The chip database

Chip parameters and the T76's per-chip FPGA bitstreams come from XGecu's own
files, which are **not redistributed here**. Point the tool at an extracted
Xgpro install:

```sh
minipro --db /path/to/Xgpro_T76 search 27C256
```

To get one: download `xgpro_T76_V*.rar` from the
[community mirror](https://github.com/Kreeblah/XGecu_Software) and extract it
**twice** — the RAR contains a WinRAR SFX executable:

```sh
unar xgpro_T76_V1321.rar        # → Xgpro_T76_V1321.exe
unar Xgpro_T76_V1321.exe        # → InfoICT76.dll + algoT76/*.alg
```

> ⚠️ Use `unar`, not `7z`. p7zip *lists* the RAR5 payload fine but cannot decode
> it — it exits with `Unsupported Method` and leaves **0-byte files** that look
> like a successful extraction.

Two ways to skip the manual extraction:

- **`--db-url <URL>`** provisions from a mirror serving the extracted files. The
  proprietary DLL is fetched to RAM and never persisted; only the derived
  catalog and bitstreams are cached.
- **`--features rar`** reads the vendor archive *in place* — point `--db`
  straight at the `.rar` or the `.exe`:

  ```sh
  cargo build --release --features rar
  minipro --db ~/Downloads/xgpro_T76_V1321.rar search 27C256
  ```

  Nothing is unpacked to disk: the DLL and each bitstream are decompressed into
  memory on demand (~0.3 s for the catalog, ~10 ms per bitstream). This is
  **off by default** because it links [unrar](https://www.rarlab.com/), which is
  not MIT and not pure Rust — enabling it puts unrar's terms on your binary,
  while the default build stays MIT-clean and C-free.

## What it does

`read` · `write` · `erase` · `info` · `search` · `detect` · `logic` ·
`autodetect` · `tui`

Across the three drivers: memory read/write/erase/blank-check/identify, MCU
fuses, JEDEC/PLD rows, write-protect, calibration, logic-IC testing, SPI
autodetect — plus eMMC/NAND, pin-contact test and firmware update on the T76.
Full capability matrix in the [`minipro-rs` README](minipro-rs/README.md).

Design notes worth knowing: reads are **verified by default** (re-read stability
plus crc32/sha256 in the outcome), and the type system encodes the T76's nastiest
hardware quirk — an undrained USB response wedges the device until replug, so the
must-drain guard makes forgetting it a compile error.

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

Codes include `usb`, `protocol`, `chip_id_mismatch`, `bad_contact`,
`overcurrent`, `firmware_mismatch`, `verify_failed`, `unsupported`, `format`,
`io`. Where a remediation exists, the outcome carries a `hint` — e.g. a `usb`
failure on macOS suggests the USB-2.0 cable.

**TUI** — a [ratatui](https://ratatui.rs) interface with a searchable chip-DB
browser, a live 40-pin ZIF contact map, an operation progress gauge, a hex view,
and a log pane.

## Repo layout

```
minipro-rs/     The Rust workspace — core / proto (drivers) / usb / db / cli
docs/           Reverse-engineering notes, protocol analysis, design docs
docs/hardware/  FPGA + MCU pinouts and schematic (Anlogic EG4X20 + WCH CH569W)
dumps/          Verified ROM dumps read with this tooling (Adaptec AHA-1542CP)
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
| [`rust-redesign.md`](docs/rust-redesign.md) · [`rust-trait-model.md`](docs/rust-trait-model.md) · [`rust-roadmap.md`](docs/rust-roadmap.md) | The design this project was built from, and what's next |
| [`minipro-vs-rust.md`](docs/minipro-vs-rust.md) | Honest comparison with the C tool |

## Contributing

The most valuable contribution is **hardware nobody here has**:

- **🔌 Run it against a real T48 or T56.** The drivers are complete and
  golden-tested but have never touched silicon. `minipro info`, `detect`, and a
  read of a known chip — reporting what you see — turns "reference-only" into
  "proven."
- **🔌 Exercise T76 write/erase/NAND/eMMC.** Reads are byte-verified; the rest is
  not.
- **Implement the TL866II+ / TL866A/CS drivers.** The TL866II+ is close to the
  existing T48 and reuses most of the shared wire layer.

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

The MIT grant covers this project's own code and documentation only. Proprietary
third-party material — XGecu's Xgpro application and `InfoICT76.dll`, vendor
datasheets, device-support lists, firmware images, and `.alg` bitstreams — is
**not redistributed here** and is not covered by it; the tooling consumes some of
it at runtime from vendor or mirror sources.
