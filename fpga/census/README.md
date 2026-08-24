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

## Differential operation map — measured 2026-08-24

Each operation run with `MINIPRO_KEEP_BITSTREAM=1` so the census stayed
resident, capturing the frames throughout. Counts are the peak reported in any
frame of the run.

| operation | bursts | words | HD lines active | our replies | HTACK | completes | wedges |
|---|--:|--:|--:|--:|--:|---|---|
| idle | 0 | 0 | 0 | 0 | 0 | — | no |
| `info` | **0** | **0** | **0** | 0 | 0 | yes | no |
| `detect` | 23 | 161 | 17 | 22 | 22 | no | yes |
| `read` | 23 | 161 | 18 | 21 | 21 | no | yes |
| **`erase`** | 21 | 147 | 17 | 21 | 21 | **yes** | **no** |
| `write` | 44 | 308 | 18 | 42 | 40 | no | yes |

**`erase` completes, and does not wedge.** Run three times consecutively, every
one exiting 0 with the T76 answering normally afterwards. It is the only
operation that finishes with an instrument in the FPGA instead of a programmer,
which says it never needs data back from the FPGA -- it issues its sequence and
takes a status. That makes it the stimulus to build future experiments on:
real HSPI traffic, repeatable, at no cost in replugs.

**`info` produces literally zero HSPI traffic** -- not one burst, not one word,
no pin moving but `HTCLK`. It is answered by the MCU alone, which makes it a
clean negative control: activity in any other operation is operation-specific
rather than background.

**`detect` and `read` are indistinguishable here** -- both 23 bursts and 161
words. They share an opening sequence and diverge only past the point where our
FPGA stops satisfying the MCU.

**`write` is almost exactly double** `detect`/`read` (44/308 against 23/161),
which suggests a second pass of the same setup rather than payload.

**None of these counts include bulk data.** 161 words is nowhere near 64 KiB, so
everything measured is command traffic. Whatever carries payload happens past
the point our FPGA can sustain.

**The per-pin signature differs by operation**, which is the most promising
thread for decoding the command encoding:

| operation | busiest HD lines (transitions) |
|---|---|
| `detect` | `HD6` 1377, `HD11` 963, `HD7` 941, `HD13` 821, `HD10` 773 |
| `erase` | `HD8` 3562, `HD11` 1973, `HD13` 1783, `HD9` 1669, `HD6` 1037 |

`HD0`/`HD1` stay low in both (225/112 and 241/120) and `HD14`/`HD15` barely move
(around 60-78), consistent with a command field in the middle lines while the
extremes carry something near-static.

## What selects the operation: one bit — measured 2026-08-24

`erase` versus `detect`, same chip, operation block window (`SKIP=63`). Nine
packets each; **exactly one differs, in exactly one bit**:

| | TSQN 15, USDF `0x24` |
|---|---|
| `erase` | `0000 `**`8000`**` 0002 0000` |
| `detect` | `0000 `**`0000`**` 0002 0000` |

Every USDF value and every other payload word is byte-identical across all
eighteen packets, on top of the nine init packets already known to match.

**This reframes the protocol.** The operation is not encoded in USDF. USDF is a
**register/table index** and the operation is selected by a control bit in the
payload written to index `0x24`. What the MCU sends is not a command stream but
a **register file being written**:

- the descending USDF runs (`0x22 0x21 0x20 0x1F`, `0x26 0x25 0x24`, `0x29 0x28`)
  are sequential register addresses, not opcodes
- the sparse single-bit payloads are individual control bits
- `0x0064` is an ordinary numeric parameter
- `0x24` recurring at packets 8, 15 and 19 with different payloads each time is
  one register being rewritten, not a command reissued

That also explains why every earlier attempt to find an opcode failed: there
isn't one. It explains why the init block is identical across operations and
chips -- it is generic register setup -- and why operations of very different
length (erase 21 packets, detect 23, write 44) share so much structure.

## The complete erase command sequence — measured 2026-08-24

Three capture windows (`SKIP` = 0, 63, 126) cover all 21 packets of an erase.
Every window was byte-identical across repeated upload-and-erase cycles.

| pkt | TSQN | USDF | payload | block |
|---:|---:|---|---|---|
| 0 | 0 | `0x22` | | init |
| 1 | 1 | `0x21` | | init |
| 2 | 2 | `0x26` | `0080` | init |
| 3 | 3 | `0x29` | | init |
| 4 | 4 | `0x20` | | init |
| 5 | 5 | `0x25` | `0020` (word 2) | init |
| 6 | 6 | `0x28` | | init |
| 7 | 7 | `0x1F` | | init |
| 8 | 8 | `0x24` | `8000` | init |
| 9 | 9 | `0x27` | | **operation** |
| 10 | 10 | `0x3C` | | operation |
| 11 | 11 | `0x0E` | | operation |
| 12 | 12 | `0x3B` | `0064` | operation |
| 13 | 13 | `0x38` | | operation |
| 14 | 14 | `0x0E` | | operation |
| 15 | 15 | `0x22`/`0x24` | | init again |
| 16 | 0 | `0x21`/`0x27` | | init again |
| 17 | 1 | `0x26` | `0080` | init again |
| 18 | 2 | `0x01` | | tail |
| 19 | 3 | `0x24` | `8000` | tail |
| 20 | 4 | `0x27` | | tail |

**Structure.** A ~9-packet init block, a ~6-packet operation block, the init
block again, then a 3-packet tail. The init block is identical across `erase`
and `detect` and across four chip capacities, so it is generic socket setup;
only the operation block should carry what distinguishes one operation from
another.

**Command ranges.** Init uses `0x1F`-`0x29` in three interleaved descending
runs. The operation block uses a different range -- `0x38`-`0x3C` plus `0x0E`,
with `0x0E` appearing twice -- and carries the only non-trivial numeric
parameter seen, `0x0064`.

**Caveat on packets 15-17.** Those straddle a window boundary and were captured
in different runs; the two readings disagree (`0x22`/`0x21` versus
`0x24`/`0x27`). Treat that row as unsettled until captured within one window.

**Correction to the handshake trade-off recorded below.** `erase` exited 0 with
the synchronised HTRDY in this window, having failed 4/4 in the previous one.
So the failure is not a clean function of the handshake choice; it is
intermittent, and the earlier characterisation was too confident.

## Packets 9-15, and the init block repeats — measured 2026-08-24

Moving the capture window with `SKIP=63` reaches the next nine packets. A
distinct USDF range appears, and the sequence number wraps exactly as 10.2.2
describes (0-15):

| TSQN | USDF | payload |
|---|---|---|
| 9 | `0x27` | |
| 10 | `0x3C` | |
| 11 | `0x0E` | |
| 12 | `0x3B` | `0064` |
| 13 | `0x38` | |
| 14 | `0x0E` | |
| 15 | `0x22` | |
| 0 | `0x21` | |
| 1 | `0x26` | `0080` |

Two things follow.

**A second command range exists.** `0x38`-`0x3C` and `0x0E` are nowhere in the
opening block's `0x1F`-`0x29`, and `0x0E` repeats at TSQN 11 and 14. The first
real numeric parameter also appears here: `0x0064` (100).

**The initialisation block repeats.** From TSQN 15 the sequence restarts with
`0x22`, `0x21`, `0x26` + payload `0x0080` -- byte-identical to packets 0, 1 and
2. So an erase issues that block twice, which accounts for 21 packets rather
than the ~15 one block takes.

**Regression, unexplained.** `erase` began failing consistently (exit 1) at this
point, having run reliably all session. Changes since the last reliable build
are capture-side only -- 16-bit words, `SKIP`, the arming logic -- and none touch
the HTRDY handshake, though every rebuild is a fresh place-and-route so timing
could have shifted. Not diagnosed. If erase reliability is needed, the build at
`b01196f` is the last one measured working.

## USDF is not an opcode — measured 2026-08-24

`erase` and `detect`, same chip, deterministic capture position: **all nine
opening packets byte-identical**, same USDF, same payloads, every CRC valid.

Two entirely different operations issue the same opening. Combined with four
chip capacities also opening identically, the reading is:

**The first ~9 packets are a fixed initialisation sequence, independent of both
operation and chip.** The descending USDF values -- `0x22 0x21 0x20 0x1F`,
`0x26 0x25 0x24`, `0x29 0x28` -- are a **table walk**, not commands, and the
sparse single-bit payloads (`0x0080`, `0x0020`, `0x8000`) are that table's
contents being loaded one entry at a time.

That also explains the packet counts measured earlier: erase 21, detect and
read 23, write 44. If roughly the first nine are shared setup, operations
diverge **later**, and everything that distinguishes them lies past a 9-packet
window. The opcode hunt was looking at the wrong end of the sequence.

## PROVEN: the packet framing, by CRC — measured 2026-08-24

With the capture position fixed, the erase command stream decodes and the
framing is confirmed rather than inferred.

**Determinism control first.** Seven upload-and-erase cycles produced seven
byte-identical captures across all 16 words, sequence field included. The same
experiment previously varied in six of twelve words. No cross-run comparison
below this line should be trusted without that control passing.

**Capture, identical for all four capacities** (32K, 64K, 128K, 256K):

```
[ 0] 0022  [ 1] C000  [ 2] 0000  [ 3] 0000  [ 4] 0000  [ 5] 0000  [ 6] 80B3
[ 7] 0021  [ 8] C400  [ 9] 0000  [10] 0000  [11] 0000  [12] 0000  [13] 4485
```

**The first packet of an erase is chip-independent** -- a fixed setup packet.
That is the properly-controlled version of the earlier "per-chip parameter
block", which was an artifact of comparing non-corresponding packets.

**Framing, proven:**

| field | value |
|---|---|
| packet | 2 header words + 4 payload + 1 CRC = **7 words** |
| header | `TLL2B[31:30]`, `TSQN[29:26]`, `USDF[25:0]` -- as datasheet 10.2.2 |
| CRC | poly `0x8005`, init `0xFFFF`, refin, refout, xorout `0xFFFF`, LE bytes |
| headers seen | `0xC0000022` then `0xC4000021` |
| TSQN | 0 then 1 -- increments per packet, as 10.2.2 describes |
| USDF | `0x22` then `0x21` -- the command field |

The CRC parameters were found by search over standard CRC-16 variants and
**match both packets simultaneously**. Two independent 16-bit values agreeing on
one parameter set is not coincidence, so the packet boundaries, the header
layout and the CRC are established fact. The datasheet gave the polynomial;
this pins the full parameterisation.

**What this unlocks.** Packets can now be parsed rather than eyeballed: split on
7-word boundaries, verify the CRC, read TSQN and USDF. Differential work moves
from "which words differ" to "which USDF values correspond to which operation
and parameter", which is the actual protocol.

## CORRECTION: the captured words are not parameters — measured 2026-08-24

Seven identical `erase W27C512@DIP28` runs, back to back, same chip, nothing
varied:

| run | [0] | [5] | [7] |
|---|---|---|---|
| 0 | `D400` | `3BC9` | `D800` |
| 1 | `E400` | `B52C` | `E800` |
| 2 | `F400` | `4DD9` | `F800` |
| 3 | `D800` | `8C15` | `C800` |
| 4 | `E800` | `5744` | `EC00` |
| 5 | `F800` | `FA05` | `FC00` |
| 6 | `C800` | `74E0` | `CC00` |

Words 0 and 7 **vary between identical runs**, cycling `C800 D400 D800 E400
E800 F400 F800` -- top nibble stepping C-D-E-F with bit 11 toggling. That is a
sequence number, which 10.2.2 says hardware increments after each successful
transmission (`TSQN`). The only words stable across all seven are [1], [3], [4],
[8], [10], [11] -- and every one is zero.

**Two findings below this section are withdrawn:**

- The **"opcode at bit 12"** reading. The `0x1000` delta between `erase` and
  `detect` lies inside the range those words wander across identical runs, so
  it is consistent with sequence drift rather than a command field.
- The **"per-chip parameter block"** reading. Word 0 across four chips gave
  `C000 D000 F400 C400`, all inside the same `C/D/E/F` x `x000/x400/x800`
  family as this noise. What looked like parameters was very likely the counter.

**Root cause.** The capture keeps the *most recent* burst and `capcnt` resets at
every burst start, so which packet is captured depends on where the run happens
to stop. Every cross-run comparison so far compared arbitrary, non-corresponding
packets and read the difference as meaning.

**The fix, and how to know it worked.** Capture a deterministic packet -- the
first burst after arming -- so the same position is compared each time. This is
the one-shot idea again, but now with a reason rather than as extra mechanism,
and with a control to verify it: seven identical runs must produce seven
identical captures. Do not trust any cross-run comparison until that control
passes.

## Reading the command words — measured 2026-08-24

Reverting the capture to exactly `a93f729` restored it. Everything added after
that commit -- the one-shot `capfull` gate, the deeper buffer, the shift
register, the windowed readout, the bidirectional HD -- was mechanism layered on
a working design, and somewhere in it the capture stopped working. Three
diagnoses in a row were wrong. A straight revert took one build.

Paired with `erase`, which completes and does not wedge, the parameter block is
now directly readable. Four chips from one family, differing only in capacity:

| idx | W27C257 (32K) | W27C512 (64K) | W27C010 (128K) | W27C02 (256K) | |
|---|---|---|---|---|---|
| 0 | `C000` | `D000` | `F400` | `C400` | differs |
| 1 | `0000` | `0000` | `0000` | `0100` | differs |
| 2-4 | `0000` | `0000` | `0000` | `0000` | constant |
| 5 | `B487` | `4C72` | `4DD9` | `8899` | differs, noise-like |
| 6 | `0001` | `0035` | `0027` | `0027` | differs |
| 7 | `C400` | `D400` | `E400` | `C800` | differs |
| 8-11 | `0000` | `0000` | `0000` | `0000` | constant |

Upper byte is the constant `0x2D` on `HD16`-`HD22`/`HD31` throughout, consistent
with the 16-bit bus measured earlier.

**Structure, inferred rather than proven.** Datasheet 10.2.2 specifies a 32-bit
header transmitted low half first, then payload, then CRC16. Word 5 behaving
like noise while 2-4 are zeros is what a CRC closing a packet looks like, which
puts the framing at **2 header + 3 payload + 1 CRC = 6 words**, word 6 opening
the next packet. Reading words 0/1 as that header places the chip-specific value
in **USDF**, the 26-bit field the datasheet reserves for the user's "command
word, address field and time stamp information" -- exactly where a vendor
command would live. None of this is confirmed against a computed CRC yet.

**One partial regularity.** Word 7 runs `C400`, `D400`, `E400` across 32K, 64K
and 128K -- **+0x1000 per doubling** -- then breaks to `C800` at 256K. Suggestive
of a size or address field; three points and an exception is not a conclusion.

**Operational notes.** The first erase after a bitstream upload reports zero
bursts; subsequent ones are reliable, so discard the first. `bursts` and
`capwords` are cumulative across runs while per-pin edge counters reset per
frame -- read deltas for the former, sums for the latter.

## Erase parameter sweep — measured 2026-08-24

Four chips from one family, same maker, varying only in capacity, each erased
with the census resident. `erase` does not wedge, so all four ran back to back
with no replug.

| chip | size | bursts per erase | words |
|---|--:|--:|--:|
| `W27C257@DIP28` | 32 KiB | 21 | 147 |
| `W27C512@DIP28` | 64 KiB | 21 | 147 |
| `W27C010` | 128 KiB | 21 | 147 |
| `W27C02` | 256 KiB | 21 | 147 |

**Command volume is independent of chip size.** Identical at every capacity, so
erase does not iterate over the device -- it hands the FPGA a parameter block
and the FPGA does the work.

**A methodology note that nearly produced a wrong result.** `bursts` and
`capwords` are cumulative: they saturate but never reset. Read naively across a
sweep they appear to scale with size (84, 105, 126, 147) and invite a tidy
story about address bits. The deltas are a flat +21 every time. Per-pin edge
counters *do* reset per frame, so they must be summed across a run rather than
maxed, or an operation spanning a frame boundary is undercounted.

**The bus partitions into framing and payload:**

| class | lines | across all four chips |
|---|---|---|
| framing | `HTREQ`, `HTVLD`, `HTACK`, `HD3`, `HD4` | constant (38-42, 30-32) |
| payload | `HD0`-`HD2`, `HD5`-`HD13` | vary 20-fold, non-monotonically |
| near-static | `HD19`, `BD7` (R5) | 2 transitions |

The variation is **not** size-correlated -- `HD8` runs 364, 36, 1556, 324 and
`HD2` runs 1486, 84, 84, 76 -- so those lines carry per-chip parameter values
(algorithm, voltages, timings) whose bit patterns differ arbitrarily between
parts, not capacity.

**The ceiling of this technique.** Transition count is a proxy for activity, not
for value: two different values of equal Hamming weight are indistinguishable.
It identifies *which* lines carry parameters and cannot say *what* they carry.
Reading the encoding needs actual captured words, which is the packet capture
that has not worked since `a93f729`.

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
