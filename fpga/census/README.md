# T76 static census — stage 1 of the FPGA-side interface inventory

Samples every board net the T76's FPGA can reach and reports three bits per
pin: its level when the frame was latched, whether it was ever low during the
preceding window, and whether it was ever high. That is enough to separate
*tied low*, *tied high* and *active* without decoding any protocol.

**Status: verified in simulation, never synthesised, never run on hardware.**
Building it needs Anlogic TD, which we do not yet have. The RTL passes a
testbench that decodes the UART exactly as the host decoder does and checks the
classification semantics; that is the whole of its provenance so far.

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

# simulate (this machine's iverilog needs an explicit -B; see note below)
iverilog -B /opt/zb/lib/ivl -g2005 -o /tmp/tb.vvp tb_census.v t76_census.v
vvp /tmp/tb.vvp

# synthesise with Anlogic TD, then convert and upload
python3 gen_bit.py T76.bit out.bit          # from radiomanV/Xgecu_T76
python3 t76_uploader.py out.bit
```

Wire `ISP:J19` (ball `E13`) to a 3.3 V USB-serial adapter's RX, with a common
ground from any `ISP:GND` pin. **115200 8N1.** A frame arrives every 250 ms.

```sh
./decode_census.py /dev/tty.usbserial-XXXX
./decode_census.py capture.bin --raw
```

## What it should settle

- **`M0` (T11) / `M1` (N11)** — the FPGA configuration-mode straps, read directly.
- **`TCK`/`TDI`/`TDO`/`TMS`** — the ball map annotates all four `(nc)`. If they
  are in fact wired, JTAG is a far better debug channel than anything we build.
- **`? ro5 ?` at ball `C4`** — unidentified in the pinout. Static or active is
  the first thing worth knowing about it.
- **How many of the 24 wired `HD` lines actually move** — which gives the bus
  width, and therefore whether the link runs 8-bit or 16-bit. 32-bit is already
  ruled out: `HD23`–`HD30` exist on the QFN68 package but are not wired.
- **HSPI vs BUS** — a free-running clock on `HTCLK` says HSPI; strobe-driven
  activity on `BWR#` with changing address lines says BUS.

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
