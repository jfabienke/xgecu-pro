# minipro-rs — roadmap

*As of 2026-08-13. Companion to [`minipro-vs-rust.md`](minipro-vs-rust.md) and
the [README](../minipro-rs/README.md) capability matrix.*

## Where we are

- **Drivers:** T48 / T56 / T76 behind one `Programmer` trait; `detect()`
  dispatches on the system-info byte. T76 reads are **hardware-proven**
  (byte-identical); T56/T48 are **reference-only** (verified against byte-exact
  golden packets — no T48/T56 silicon on hand).
- **Ops implemented (all three):** memory read/write/erase/blank/identify,
  fuses, JEDEC rows, protect, calibration (T56/T76), logic test, SPI autodetect.
  T76 adds eMMC/NAND, pin-test, firmware-update (transport only).
- **Database:** `DllDb` (parses `InfoICT76.dll`), `HttpDb` (mirror-provisioned,
  versioned catalog), `XmlDb` (+ `logicic.xml` vectors). Utility bitstreams
  (`TestLgcPull`, `TTL1`, `SPI25F*`) fetch by name from `algoT76/`.
- **CLI:** read/write (raw/ihex/srec), erase, info, search, detect, logic,
  autodetect, TUI. **164 tests, clippy-clean.**
- **License:** MIT. The protocol subsystems a review flagged as C-shaped were
  reimplemented as independent expression of the wire facts (byte-exact vs the
  goldens); the reverse-engineering credit for those facts is in the repo
  [`NOTICE`](../NOTICE).

Legend: **S/M/L** effort · 🔌 needs hardware to complete or validate · ⛓️ has a
dependency.

## Phase 1 — CLI completeness (software, no hardware)

The protocol ops exist but several aren't reachable from the CLI. Highest
value-per-effort, all doable now.

| Item | Notes | Effort |
|---|---|---|
| `firmware <file>` verb (T76) | `FirmwareUpdate` impl already exists; add the CLI verb + confirmation + post-flash re-enumeration | S |
| Multi-region read/write | Expose **data / user / config** spaces, eMMC **partitions**, and page selection (driver ops largely exist; the CLI drives code memory only) | M |
| `fuses` verb | Read/write MCU fuse/config/lock. ⛓️ needs the per-chip fuse descriptor from Phase 2 | M |
| `jedec` / PLD verb + `.jed` file `Format` | GAL/PLD fuse rows + JEDEC fuse-map file I/O. ⛓️ needs Phase 2 | M |

## Phase 2 — GAL/PLD data fidelity (audit #3, unblocks #1)

Niche (PLD/GAL parts) but it's the prerequisite for the fuse/JEDEC CLI to be
correct.

| Item | Notes | Effort |
|---|---|---|
| Parse `config` / `gal_config_t` | Fuse-map geometry into `Device`; unblocks real GAL programming and the fuse/JEDEC verbs | L |
| Host-side `pin_map` tables | Per-package socket→pin arrays for pin-test *reporting* fidelity (cosmetic; no read/write impact) | S |

## Phase 3 — Memory-region breadth (audit #2)

| Item | Notes | Effort |
|---|---|---|
| `Data2` / `Config` memory spaces | Currently `Unsupported` in all three drivers; wire the protocol + identify which chips use them | M |

## Phase 4 — Firmware update (audit #5)

Low-frequency, hardware-risky, and the per-device updaters are obfuscation-table
transcription that can't be verified without hardware + a real `updateT*.dat`.

| Item | Notes | Effort |
|---|---|---|
| T48 / T56 firmware updaters | Per-device; the T76 impl is the template | M each 🔌 |

## Phase 5 — Remaining drivers

| Item | Notes | Effort |
|---|---|---|
| TL866II+ | II+-class, ~T48 shape (EP02 bulk, fixed-silicon); reuses the shared `wire` layer | M |
| TL866A/CS | The outlier: 24-bit addressing, alternate opcode space, latch-based ZIF, no digital voltage control; won't reuse `pack_begin64` wholesale | L |

## Phase 6 — Hardware validation 🔌 (audit #6, #7, #8)

Gated on physical programmers + test chips. Do opportunistically — a T76 and the
vendor DB are already set up, so **socketed** T76 write/erase can be validated
any time a chip is in the ZIF.

| Item | Notes |
|---|---|
| T76 write / erase | Socketed SPI/parallel part → write-back-verify |
| T76 NAND / eMMC | Needs the NAND/eMMC socket adapters |
| T76 firmware update, USB reset/re-enumeration | The `firmware`/`reset` paths carry `TODO(hw)` |
| T48 / T56 end-to-end | Reference-only today; needs the actual programmers. Resolves the write-buffer-size `TODO(hw)` (#8) |
| TUI read flow | Run on hardware (its own `TODO(hw)`) |

## Explicit non-goals (off the roadmap)

- **Host-side bit-bang / pin-driver subsystem** — vintage parallel PROMs are read
  natively by the FPGA drivers via ROM-class algorithms; this only benefits
  fixed-silicon and duplicates in software what the FPGA does.
- **Compiled/baked-in database** — direct-to-source (`DllDb`/`HttpDb`) keeps the
  catalog live and is strictly better than a frozen blob.
- `Txn::drop` `end()`-error swallowing — intentional best-effort de-energize.

## Recommended sequence

1. **Phase 1 quick wins first** — the `firmware` verb (impl already exists) and
   multi-region CLI give broad value with no hardware and no new subsystems.
2. **TL866II+ (Phase 5)** — cheap coverage win; it's close to the T48 and the
   shared `wire` layer already handles most of it. Validates the trait design
   against a fourth programmer.
3. **Phase 2 → the fuse/JEDEC CLI (Phase 1 tail)** — the GAL/PLD path, when a
   PLD/GAL use case actually appears; niche, so not urgent.
4. **Hardware validation (Phase 6)** — continuous/opportunistic; each new op
   should get a socketed check as chips are available.
5. **TL866A/CS + firmware updaters** last — highest effort, narrowest immediate
   payoff, and (for firmware) hardware-unverifiable.
