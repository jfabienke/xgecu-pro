# T76 static census — stage 1 of the FPGA-side interface inventory

Samples every board net the T76's FPGA can reach and reports three bits per
pin: its level when the frame was latched, whether it was ever low during the
preceding window, and whether it was ever high. That is enough to separate
*tied low*, *tied high* and *active* without decoding any protocol.

**Status: running on hardware. Stage 2 complete: the link is plaintext and 16-bit.** The RTL passes a
testbench that decodes the UART exactly as the host decoder does and checks the
classification semantics, and Anlogic TD 4.6.116866 takes it through to a
placed, routed bitstream for `eagle_20` / `BGA256X` using **549 LUTs and 727
flip-flops, about 5% of the device**. It has never been loaded onto a T76.

Three independent cross-checks say the bitstream is the right shape:

| Quantity | prjtang `devices.json` | our TD build |
|---|---|---|
| frames | 1259 | 1259 |
| bits per frame | 3904 (488 B) | 3904 (488 B) |
| idcode | `0x00014c35` | `0x014C35` |

and after `gen_bit.py` conversion the T76-format file is **622,130 bytes —
exactly the length of radiomanV's known-working `out.bit`**.

## Why this instead of a targeted sniffer

The MCU↔FPGA link was identified as CH569 **HSPI** by mapping the 33 CPU-facing
balls through the CH569W pinout: `HD0`–`HD22` plus `HD31`, `HTCLK`/`HRCLK`,
`HTREQ`/`HTACK`, `HTVLD`/`HRVLD`, `HTRDY`/`HRACT`. Building a sniffer against
that assumption would have been premature, because the *same* 33 balls are also
a complete CH569 8080-style parallel **BUS** interface — `BD0`–`BD7`,
`BA0`–`BA5`/`BA13`, and `BWR#` — and the single CPU ball with no HSPI function
at all (`R5`, CH569 pin 13) is `PA7/`**`BD7`**`/RXD1`. HSPI remains the leading
hypothesis, since radiomanV's design consumes a free-running ~80 MHz clock on
`E8`, which is `HTCLK`, and the BUS interface has no continuous clock. But a
census settles it by observation rather than inference.

The documentation we have is also demonstrably incomplete: the T76 ball map
labels only `CLK_20` as a clock, while `E8` — used as an 80 MHz clock input by a
design that provably runs — is labelled `Cpu: 56`.

## Safety

**Run with the ZIF socket empty.** The rail-control lines (`T:OE_*`, `T:LE_*`)
switch VPP/VCC/GND onto the socket. This design drives them to a static state
rather than leaving them floating, but their polarity is undocumented, so the
"off" guess is not guaranteed. With an empty socket that does not matter.

**Every observed pin is declared `input`, never `inout`.** The toolchain
therefore cannot infer a driver, so the FPGA cannot contend with the MCU. This
is structural rather than a matter of care, and it is the one rule that must
survive any edit. The constraints set `PULLTYPE = NONE` and no `DRIVESTRENGTH`
on all 113 observed pins for the same reason.

Bitstreams are volatile and loaded per operation, so a bad one is recovered by
power-cycling the programmer.

## Files

| File | Role |
|---|---|
| `gen_census.py` | **Single source of truth.** Reads `docs/hardware/fpga_t76_pinout.ods` × `CH569W_pinout.ods` and generates everything below |
| `census_ports.vh`, `census_obs.vh`, `census_tb_connect.vh` | generated Verilog port list, observation vector, testbench wiring |
| `t76_census.adc`, `t76_census.sdc` | generated TD pin and timing constraints |
| `pinmap.json` | generated index → {ball, net, signal} table |
| `t76_census.v` | the design (hand-written) |
| `tb_census.v` | testbench (hand-written) |
| `decode_census.py` | host decoder |

Never hand-edit the generated files — re-run `gen_census.py`. The constraints,
the RTL port order, and the host decode table all index pins identically
*because* they come from one generator, which is the only reason the decoder can
be trusted to name the right ball.

## Build and run

```sh
python3 gen_census.py                       # regenerate after any pinout fix

# simulate (this machine's iverilog needs an explicit -B; see notes below)
iverilog -B /opt/zb/lib/ivl -g2005 -o /tmp/tb.vvp tb_census.v t76_census.v
vvp /tmp/tb.vvp

# synthesise (TD 4.6.8 lives in the td-x86 OrbStack machine; see fpga/TOOLCHAIN.md)
orb -m td-x86 bash -lc 'cd ~/census && TD_HOME=/opt/TD /opt/TD/bin/td < build.tcl'

# convert to T76 format and upload
python3 gen_bit.py t76_census.bit t76_census_out.bit   # radiomanV/Xgecu_T76
python3 t76_uploader.py t76_census_out.bit
```

`t76_census_out.bit` is committed, so a T76 owner can run the census without
Anlogic TD at all.

### What TD needed, that cost time

Recorded because none of it is documented and all of it recurs:

- **`elaborate` segfaults.** `elaborate -top <name>` dies in
  `FeToRtl::SetData`. It is also unnecessary: `read_verilog` already reports
  `HDL-1200 : Current top model is t76_census`.
- **Use prjtang's flow, not `map`/`pack`.** The working order is
  `optimize_rtl` → `optimize_gate` → `legalize_phy_inst` → `place` → `route` →
  `bitgen`. Going through `map`/`pack` instead leaves adders and comparators
  with "missing physical model" at placement.
- **`bitgen` wants `-version 0X00 -g ucode:000000000000000000000000`.**
- **No reset network.** An outer `if (rst)` makes TD infer a dedicated
  set/reset pin per flop, and it then refuses to pack any flop needing both —
  a constant-1 load is a *set*, the reset is a *reset* — aborting with
  `SYN-8700` inside `pack::SeqSetReset`. Rely on declared initial values, which
  the bitstream loads at configuration. radiomanV's working design does the
  same.
- **`/bin/sh` must be bash.** TD shells out with bash-isms and Ubuntu's dash
  emits `sh: Syntax error: Bad fd number` throughout.
- **The licence goes at `/opt/TD/license/Anlogic.lic`** — that exact path and
  filename. The `license.lic` published under *TD License* works despite naming
  its feature `FD`, and carries `HOST_ID = ANY`.
- **TD is a TCL shell that reads stdin.** There is no `-f script` flag; `td -f
  build.tcl` silently waits on stdin forever. Use `td < build.tcl`.

Wire `ISP:J19` (ball `E13`) to a 3.3 V USB-serial adapter's RX, with a common
ground from any `ISP:GND` pin. **115200 8N1.** A frame arrives every 250 ms.

```sh
./decode_census.py /dev/tty.usbserial-XXXX
./decode_census.py capture.bin --raw
```

## What it should settle

- **`M0` (T11) / `M1` (N11)** — the FPGA configuration-mode straps, read directly.
- **`? ro5 ?` at ball `C4`** — unidentified in the pinout. Static or active is
  the first thing worth knowing about it.
- **How many of the 24 wired `HD` lines actually move** — which gives the bus
  width, and therefore whether the link runs 8-bit or 16-bit. 32-bit is already
  ruled out: `HD23`–`HD30` exist on the QFN68 package but are not wired.
- **HSPI vs BUS** — a free-running clock on `HTCLK` says HSPI; strobe-driven
  activity on `BWR#` with changing address lines says BUS.

## The encryption question, answered — measured 2026-08-23

**The MCU↔FPGA HSPI link carries plaintext. It is not AES or SM4 encrypted.**

Captured by asserting `HTRDY` and recording `HD[23:0]` in the `HTCLK` domain
while `HTVLD` was high, during a 64 KiB uniform write driven by `minipro-rs`
with `MINIPRO_KEEP_BITSTREAM=1`:

| Measurement | write of `0x00` | write of `0xFF` | AES/SM4 would give |
|---|---|---|---|
| words captured | 1440 | 896 | |
| **Shannon entropy of the 16-bit data** | **2.78 bits** | **2.65 bits** | **~16.0 bits** |
| distinct values seen | 40 | 15 | ~65536 |
| words exactly `0x0000` | 810 (56%) | 504 (56%) | ~0 |

Ciphertext is indistinguishable from random by construction. A link carrying it
cannot produce 2.7 bits of entropy, cannot repeat one value 810 times out of
1440, and cannot confine 1440 samples to 40 distinct values. The ECDC module
(CH569 datasheet Chapter 15) is not enabled on this path, so the risk that would
have ended the reverse-engineering effort does not exist.

**The bus is 16-bit.** `HD16`–`HD22` and `HD31` held the constant `0x2D`
across *every* captured word in both runs, so they are wired but carry no data;
the payload is `HD0`–`HD15`. That is consistent with the T76 wiring only 23
of the 32 available `HD` lines, and it rules out the 32-bit mode outright.

**The handshake is confirmed end to end**: `HTREQ` → (we drive) `HTRDY`
→ `HTVLD` → packet, exactly as CH569 §10.2.3 describes. Before the
responder existed the MCU raised `HTREQ` and stalled forever; with it, 44 bursts
and 308 words crossed the link.

This also settles HSPI versus the 8080-style BUS reading. A BUS write strobes
`BWR#` and puts data out unconditionally; what we measured waits on a ready line
and then streams packets under a valid signal.

## First hardware run — measured 2026-08-23

24 consecutive frames, every CRC valid, **every frame byte-identical and not one
pin varying**. The T76 was idle: powered, enumerated, our bitstream loaded, no
operation in flight.

**The MCU link is silent except for its clock.**

| State | Count | Signals |
|---|--:|---|
| **ACTIVE** | **1** | **`HTCLK` (E8)** |
| tied high | 18 | `HD0`–`HD7`, `HD9`–`HD13`, `HD16`, `HD18`, `HD19`, `HD21`, `HRVLD` |
| tied low | 13 | `HD8`, `HD14`, `HD15`, `HD20`, `HD22`, `HD31`, `HRACT`, `HRCLK`, `HTACK`, `HTRDY`, `HTREQ`, `HTVLD`, `PA7/BD7/RXD1` |

`HTCLK` toggling while every data and handshake line sits static is exactly what
an idle-but-clocked link looks like, and it settles the question radiomanV's
design raised: the ~80 MHz he consumed as a free-running clock **is** the HSPI
transmit clock, and it runs whether or not a transfer is in progress. **0 of 24
wired `HD` lines are active at idle**, which is expected — nothing has asked the
MCU to move data yet. Distinguishing HSPI from the 8080-style BUS reading needs
traffic, so that waits on stage 2.

**Answers to the open questions:**

- **`M0` (T11) = low, `M1` (N11) = high** — the FPGA's configuration-mode straps,
  read directly rather than inferred.
- **`? ro5 ?` (C4)** — the ball nobody could identify is **statically high**. Not
  a signal: a pull-up or a strap.
- **Two anomalies worth a second look.** With an empty socket, 47 of 48 ZIF pins
  read high and **`zif_07` (F15) reads low**; on the ISP header 19 read high and
  **`j_25` (E3) reads low**. Whatever pulls those two differs from all their
  neighbours.

The UART transmits on **`ISP:J08` (ball M4)**, not J19. J08 is the position the
beacon proved reachable on the bench; J19 was the original choice and was never
confirmed on hardware. All five switchable ISP grounds (11, 21, 26, 27, 28) are
asserted, so the probe's ground lead can land anywhere along the header — those
five positions are consequently not observed.

## What TD told us before the bitstream even built

`import_device` prints the device's dedicated-pin map, and it settled two
questions for free:

- **Ball `T2` is `program_b`.** Our ball map calls that net `Cpu: 33`, which
  mapped to `HD17` through the CH569 pinout — but on the FPGA side it is the
  configuration-reset input. So that wire is the MCU's **FPGA-reconfiguration
  control line, not a bus data line**, and the count of wired HSPI data lines
  drops from 24 to **23**. It is excluded from the census because TD refuses to
  place user IO there (`USR-8027`).
- **`TCK`/`TDI`/`TDO`/`TMS` (C14/C12/E14/A15) are dedicated JTAG pins.** User
  logic cannot observe them at all, so the census cannot say whether the T76
  routes them. That question needs continuity testing on the board instead.

Also worth noting: `cso_b`/`cclk`/`mosi`/`miso`/`dout` (T3/R11/T10/P10/M14) and
`done` (P13) are configuration pins set to **gpio** mode, so they *are*
observable — several of them carry `HD` nets.

## Next: differential operation mapping

Planned, and it needs **no rebuild** — the committed census already measures
everything required. The idea is to use the host as a controlled stimulus: run
each operation in turn with `MINIPRO_KEEP_BITSTREAM=1` so the census stays
resident, capture the frames, and diff which pins move between operations.

```sh
# census must already be loaded; each run wedges the T76, so replug between them
MINIPRO_KEEP_BITSTREAM=1 minipro info                    # MCU only, expect no HSPI
MINIPRO_KEEP_BITSTREAM=1 minipro detect --skip-pincheck  # ID read
MINIPRO_KEEP_BITSTREAM=1 minipro read   --skip-pincheck W27C512@DIP28 /tmp/r.bin
MINIPRO_KEEP_BITSTREAM=1 minipro write  --skip-pincheck --force W27C512@DIP28 /tmp/zeros.bin
MINIPRO_KEEP_BITSTREAM=1 minipro erase  --skip-pincheck W27C512@DIP28
```

What it should yield, from data we can already collect:

- **Which pins are specific to which operation.** `info` should show no HSPI
  traffic at all (it is answered by the MCU alone); everything else should. The
  difference isolates the pins that carry operation setup.
- **Burst and word counts per operation**, which bound the command length: a
  read of 64 KiB versus an erase should differ enormously if payload crosses the
  link, and barely at all if only commands do.
- **Whether ZIF pins ever move.** With our bitstream loaded the FPGA does not
  drive the socket, so any ZIF activity would mean something else does.
- **Ordering.** `bursts` and per-pin transition counts across operations give a
  coarse sequence without needing packet capture.

Two things to know before starting:

- **Each operation wedges the T76** and needs a physical replug, because our
  FPGA acknowledges but never satisfies the MCU. Budget one replug per data
  point and capture the UART for the whole run.
- **Extending the census past ~90 counted pins needs a 9-bit `byte_idx`.** With
  99 counters the frame reaches 260 bytes and the 8-bit index wraps at 255,
  emitting garbage that decodes as scrambled pin classification. Found the hard
  way; the committed build counts 32 pins and is safely under the limit.

## Known discrepancy

`j_07` and `j_08` are **swapped** between the two sources: our ball map says
`M5`/`M4`, radiomanV's constraints say `M4`/`M5`. His design never exercised
either pin, so neither source is confirmed. The generator follows the ball map.
Treat those two indexes as provisional until something drives one and observes
which header pin moves.

## Note on this machine's toolchain

`iverilog` from `zb` has its internal base path truncated to `/opt/zb` and fails
with `sh: /opt/zb/ivlpp: No such file or directory`. Pass `-B /opt/zb/lib/ivl`
explicitly. This is the bottle path-rewriting failure mode that `zb` is known
for on short prefixes.
