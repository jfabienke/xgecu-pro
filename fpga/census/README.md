# T76 static census — stage 1 of the FPGA-side interface inventory

Samples every board net the T76's FPGA can reach and reports three bits per
pin: its level when the frame was latched, whether it was ever low during the
preceding window, and whether it was ever high. That is enough to separate
*tied low*, *tied high* and *active* without decoding any protocol.

**Status: builds to a real bitstream; never run on hardware.** The RTL passes a
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
