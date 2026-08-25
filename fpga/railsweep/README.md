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

## Next

Phase 2 is walking-ones: with polarity known, walk a single `1` through the
chain and watch which socket position grounds at each step. That gives the
bit-to-position mapping directly, and the step index is already carried in the
UART frame. VCC and VPP polarity confirmation comes after, and should be assumed
active-low until measured — with the socket empty.
