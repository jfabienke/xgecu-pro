# galsweep — reverse-engineer a combinatorial GAL or PAL from outside

A secured GAL still **works**. The security fuse blocks readback, not operation,
so RE is not about defeating the fuse — it is about characterising behaviour
from outside. Drive every input combination, record every output, hand the truth
table to a host that can minimise it.

**Status: engine, telemetry and command path validated on hardware with an empty
socket. No chip has been read yet, and the rails are not routed.**

## The pin map is a product of this project's measurements

GAL16V8 bottom-justified in the 48-pin socket. Derived from the socket's DIP
numbering and from `Z24` being permanently grounded and outside the shift chain
— not from the vendor pinout:

```
   chip  1..9   ->  ZIF15..ZIF23   driven      9 inputs
   chip 11      ->  ZIF25          driven      1 input  (/OE or I)
   chip 10      ->  ZIF24          GND -- permanently grounded, nothing to do
   chip 12..19  ->  ZIF26..ZIF33   sampled     8 I/O
   chip 20      ->  ZIF34          VCC -- needs BEGIN_TRANS from the MCU
```

`Z24` landing exactly on the GAL's ground pin is the socket being built around
the convention these families follow, and it means a 16V8 seats without a fight.

## Commanded over the wire, not rebuilt

A UART receiver on ISP pin 9 takes single-character commands:

```
   'S'   start a sweep
   '?'   status
```

That is the point of the design, not a convenience. Every parameter change used
to cost a TD build and a physical replug, and that loop has consumed more of
this work than the measurements have. Round trip verified: three `?` sent, three
`OK` returned, through host → USB → Pico PIO TX → ISP 9 → FPGA → ISP 7 → Pico
PIO RX → USB.

## Output

```
   D000 FF FF FF FF FF FF FF FF FF FF FF FF FF FF FF FF
   ...  64 lines of 16 bytes = 1024 vectors
   D3F0 FF FF FF FF FF FF FF FF FF FF FF FF FF FF FF FF
   END chg=00
```

`chg` is **direction discovery for free**: a bit set means that sampled pin
changed at least once across the sweep, so it is carrying a function. A pin that
never moved is an input, an unused output, or a constant — and the host needs to
know which of the eight matter before it minimises anything.

The run above is with an **empty socket**: every pin floats to its pull-up, so
`FF` everywhere and `chg=00` is the correct answer, not a null one.

## Timing

1024 vectors at 8 cycles each is **410 us of sweeping**, against 6 ms to report
it at 1.84 Mbaud. The measurement is never the cost; the readback is, and even
that is now interactive.

`SETTLE` is 8 cycles — 400 ns against a GAL16V8-25's 25 ns propagation. An order
of magnitude of margin, chosen because **the socket driver network's bandwidth
has never been characterised**. That is an assumption, not a measurement, and it
is the one number here that could be wrong.

## Not done

- **Powering the part needs two halves, and both now exist.** The FPGA routes
  supplies to socket pins through its shift register, but the MCU switches them
  on and sets their level: `BEGIN_TRANS` carries `raw_voltages` from the chip's
  algorithm, and `end()` de-energizes. A bitstream that routes VCC to a pin
  correctly produces nothing when the rail behind it is off -- which is exactly
  what measuring one showed, in both polarities.

  `minipro energize` closes it:

  ```sh
  MINIPRO_KEEP_BITSTREAM=1 minipro bitstream fpga/galsweep/t76_galsweep_out.bit
  MINIPRO_KEEP_BITSTREAM=1 minipro energize GAL16V8 --hold
  ```

  The MCU applies the GAL's voltages while our instrument stays resident.
  Verified: "Socket energized for GAL16V8" with `algorithm="GAL16A1"` kept
  rather than uploaded. Note the plain device name -- there is no `@DIP20`
  suffix for these parts.

  What is still untested is whether the FPGA's own VCC latch then routes that
  live rail to `Z34`. The supply exists now; the routing is unproven.
- **I/O direction is discovered, not configured.** Pins 12-19 can be inputs on a
  real part, in which case the input space is larger than 1024 and those pins
  must be driven rather than sampled. The command path exists to make that a
  parameter rather than a rebuild.
- **Registered parts** need clock-and-observe with a state walk. Same rig, same
  rails, different algorithm.
