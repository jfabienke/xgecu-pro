# railsweep — what the socket's rail control actually does

The ZIF socket's per-pin drivers hang off a 595-style chain: `ser_clk`/`ser_data`
shift a pattern in, a per-domain latch enable captures it, and a per-domain
output enable drives it onto the socket. Bit order and both polarities were
unknown, and guessing wrong is how 42 socket positions came to be recorded as
"isolated" when our own bitstream was grounding them.

## The result

Four states, cycled every two seconds, each announced over the ISP header so
every reading is self-tagging. An RP2040 in the socket reads levels:

| state | register | `gnd_oe` | socket reads |
|---|---|---|---|
| S00 | all-zero | 0 | high |
| S01 | all-zero | 1 | high |
| **S02** | **all-one** | **0** | **grounded** |
| S03 | all-one | 1 | high |

**A register bit of `1` selects a position. The output enables are ACTIVE LOW —
`0` drives, `1` releases.**

That is one measurement, not an inference, and it settled a question that had
blocked the socket work all day. Changing three constants from `0` to `1` in the
beacon took a placement from 2 positions identified to **16 of 16, without
moving the board.**

## Why GND first

Grounding a socket pin is the least dangerous thing this board can be asked to
do — no VCC, no VPP, nothing above 3.3 V anywhere in the experiment. VPP is
12 V+ and gets characterised only now that the register is understood.

## Why the ZIF pins are not declared

The FPGA drives nothing in the socket during a sweep. That is not tidiness: the
beacon drives all 48, and a ground switch closing on a driven pin is an FPGA
output fighting a ground switch. The census source warns about exactly this, and
it had been happening continuously.

## Simulation

`tb_railsweep.v` polices the things that would be expensive to discover on
hardware, and all of them passed before synthesis:

- exactly 48 shift clocks per state
- `gnd_oe` never asserted while the register is half-shifted
- latch enable and output enable never overlapping
- no VPP or VCC control moving at all during phase 1

## Phase 2 — the bit-to-position map

Walking a single `1` through the chain, across four board placements. The step
index rides in the UART frame, so every observation is tagged with the pattern
that produced it.

**Chain length is 48, and that is measured.** It was briefly widened to 64 on
the theory that the chain carried control bits beyond the socket. Every step's
mapping moved by exactly 16 — which is what over-shifting a 48-bit chain by 16
looks like, the surplus falling out the far end. A longer chain would have
produced new positions at the new steps. It produced none.

That gives the conversion, which agrees on all 42 observed bits across four
placements and two chain widths:

```
    chain position = NBITS - step
```

### The chain, complete

```
   chain 00-07  ->  Z01 Z08 Z07 Z06 Z05 Z04 Z03 Z02      Z01..Z08
   chain 08-15  ->  Z09 Z12 Z11 Z10 Z16 Z15 Z14 Z13      Z09..Z16
   chain 16-23  ->   --  Z17 Z18 Z19 Z20  --  Z22 Z21     Z17..Z22
   chain 24-31  ->  Z32 Z29 Z30 Z31 Z28 Z27 Z26 Z25      Z25..Z32
   chain 32-39  ->  Z33 Z37 Z38 Z39 Z40 Z34 Z35 Z36      Z33..Z40
   chain 40-47  ->  Z41 Z45 Z46 Z47 Z48 Z44 Z43 Z42      Z41..Z48
```

**46 of 48 positions are ground-switchable and mapped. Two are not.**

The chain runs in groups of eight, each covering a run of consecutive socket
positions, scrambled within the group and not the same scramble twice. Five
groups are complete. The sixth is the exception: bits 16-23 cover only
`Z17`-`Z22`, with **two bits driving no socket position at all** and `Z23`/`Z24`
absent from the chain entirely. The chain allocates eight bits per group whether
or not all eight positions are switchable.

Three positions were fixed by elimination rather than observed -- a group with a
single hole has one position left to put in it -- giving `chain 06 -> Z03`,
`chain 29 -> Z27`, `chain 42 -> Z46`.

### Z23 and Z24 are not in the ground chain

Established with `fpga/railhold`: all 48 bits set, enable asserted, held
**indefinitely** rather than for a four-second sweep step. Fourteen positions
grounded; `Z23` and `Z24` stayed high while their immediate neighbours `Z20` and
`Z21` went low in the same reading.

That killed the boring explanation. The walking sweep had not missed them --
there was nothing to miss. Static also let a **meter** read those positions
directly, independent of the pull probe, the decoder, and contact timing, all
three of which produced confident wrong answers during this work.

`Z22` was a false member of that set. It read as missing only because it sat
under one of the Pico's four ground pins in every placement that reached it; one
position's move put it on a live pin and it grounded immediately, at chain bit
22. **A gap in coverage looked exactly like a gap in the hardware** -- which is
the same failure that made 42 grounded positions look isolated.

### On cross-checks

Every bit in the map was seen from at least two board placements at different
offsets, and the final runs happened to use a 64-bit shift while the chain is
48, which over-shifts harmlessly and shifts every step index by 16. That
accident gave a third independent width to check against: `chain position =
NBITS - step` reconciled all three, and no bit disagreed.

## Next

VCC and VPP polarity, which should be assumed active-low by family until
measured — **with the socket empty**, because VPP is 12 V+.
