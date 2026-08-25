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

### The chain is organised in groups of eight

Sorted by chain position rather than step, the structure is immediate:

```
   chain 00-07  ->  Z01 Z08 Z07 Z06 Z05 Z04  __  Z02      = Z01..Z08
   chain 08-15  ->  Z09 Z12 Z11 Z10 Z16 Z15 Z14 Z13      = Z09..Z16
   chain 16-23  ->   __ Z17 Z18 Z19 Z20  __   __ Z21      = Z17..Z24
   chain 24-31  ->  Z32 Z29 Z30 Z31 Z28  __ Z26 Z25      = Z25..Z32
   chain 32-39  ->  Z33 Z37 Z38 Z39 Z40 Z34 Z35 Z36      = Z33..Z40
   chain 40-47  ->  Z41 Z45  __  Z47 Z48 Z44 Z43 Z42     = Z41..Z48
```

Every group is a complete run of eight consecutive positions. The order *within*
a group is scrambled, and not the same scramble in each group — so the grouping
is a fact and the permutation is not yet a rule.

**That closes three gaps by elimination.** A group with a single hole has only
one position left to put in it:

```
   chain 06 -> Z03        chain 29 -> Z27        chain 42 -> Z46
```

**45 of 48 determined**: 42 measured directly, 3 forced by the structure.

### Three positions that do not ground

`Z22`, `Z23` and `Z24` belong to chain bits 16, 21 and 22. Those steps ran in
both the 48-bit and 64-bit sweeps, with `Z23` on pin 02 and `Z24` on pin 01, and
the beacon confirmed both contacts at 32 frames minutes before each run.
**Neither ever grounded.**

So either those socket positions are not in the ground chain, or those chain
bits drive something else. Recorded as an open question rather than guessed at:
the group structure says they should be there, and the measurement says they are
not, and that disagreement is the interesting part.

## Next

VCC and VPP polarity, which should be assumed active-low by family until
measured — **with the socket empty**, because VPP is 12 V+.
