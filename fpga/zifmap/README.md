# zifmap — map the T76 ZIF socket from inside the socket

An RP2040 that sits in the T76's ZIF socket, listens to the `t76_beacon`
bitstream, and reports **which FPGA ball drives which physical socket
position**.

Until now the ZIF mapping was third-party: `docs/hardware/fpga_t76_pinout.ods`
says ball `B15` is `ZIF01`, but that came from radiomanV's board tracing and
**nothing here had ever confirmed it**. This measures it.

## Status

**Working, and it has produced results.** Six ZIF socket positions measured,
the socket's numbering convention established, and the ISP header confirmed
against radiomanV's pinout. See [Results](#results).

Output goes over the Pico's **own USB CDC port**; it needs no debug probe.
That was not the original design — SWD failed five times in one bench session,
every time immediately after the rig was handled, while the Pico's USB never
failed once. Flashing goes over UF2 for the same reason. Nothing in the loop
depends on a probe any more.

Two self-tests run at boot, and both exist because a silent instrument and a
quiet wire look identical:

- **decoder** — synthesises a `Z07\r\n` capture at the exact bit timing the
  field decode uses and decodes it. 32/32 frames, zero framing errors.
- **pads** — checks every channel's input pad with the internal pulls, after
  this board was once inserted with `VBUS` and `3V3_EN` on live socket
  contacts. All 16 healthy.

## Results

### The socket uses standard DIP numbering — measured, not assumed

Five placements, each self-consistent, and the two rows run in **opposite
directions**:

| Row | Relation | Positions |
|---|---|---|
| one side | `ZIF = pin + offset` (ascending) | 25–48 |
| other side | `ZIF = k − pin` (descending) | 1–24 |

Confirmed across offsets +24, +25, +28 on the ascending row and `25 − pin`,
`21 − pin` on the descending one. Every live position reappeared on a
*different* Pico pin at each placement, which is what makes the result
positional rather than an artefact of how the board was sitting.

### Only six socket contacts reach the FPGA with the rails off

All 48 positions measured, across ten placements on both columns and both board
orientations:

```
                       ▲  L A T C H  ▲
            ┌──────────────────────────────────┐
   Z01 · ┤▫                              ▫├ · Z48
   Z02 · ┤▫                              ▫├ · Z47
   Z03 · ┤▫                              ▫├ · Z46
   Z04 · ┤▫                              ▫├ · Z45
   Z05 · ┤▫                              ▫├ ● Z44
   Z06 · ┤▫                              ▫├ · Z43
   Z07 · ┤▫                              ▫├ · Z42
   Z08 · ┤▫                              ▫├ · Z41
   Z09 · ┤▫                              ▫├ ● Z40
   Z10 ● ┤▫                              ▫├ · Z39
   Z11 · ┤▫                              ▫├ · Z38
   Z12 · ┤▫                              ▫├ · Z37
   Z13 · ┤▫                              ▫├ · Z36
   Z14 · ┤▫                              ▫├ · Z35
   Z15 · ┤▫                              ▫├ · Z34
   Z16 · ┤▫                              ▫├ · Z33
   Z17 · ┤▫                              ▫├ ● Z32
   Z18 · ┤▫                              ▫├ · Z31
   Z19 · ┤▫                              ▫├ · Z30
   Z20 · ┤▫                              ▫├ · Z29
   Z21 · ┤▫                              ▫├ · Z28
   Z22 · ┤▫                              ▫├ · Z27
   Z23 ● ┤▫                              ▫├ · Z26
   Z24 ● ┤▫                              ▫├ · Z25
            └──────────────────────────────────┘

   ● reaches the FPGA      · isolated behind the driver network
   confirmations: Z10 x5  Z23 x3  Z24 x2  Z32 x3  Z40 x5  Z44 x4
```

**Six live, forty-two isolated**, with every rail control (`vcc_oe`, `vpp_oe`,
`gnd_oe`, the shift register, the latches) held at `0`. The isolated contacts
float, which the Pico's pull-downs read as a stable hard low.

Left/right in the diagram is arbitrary: the measurements fix each column's
numbering *direction* and which end the latch is, but nothing in the data says
which physical column is which. If it is mirrored, the columns swap and
everything else holds.

This was mistaken for bad socket contact for hours. Pressing the board down and
nudging it sideways changed *nothing* — the same positions answered every time.
**A mechanical fault varies when you disturb it. This did not.** That
distinction is the finding, and it is what the sparse result actually means.

One position, `Z21`, reported `activity, no framing` on a single pass and then
read cleanly isolated when a different Pico pin sat on it. Recorded as
isolated; the one anomalous reading was most likely crosstalk from a
neighbouring channel. Repeating a measurement on different hardware is what
settled it.

**An untested hypothesis about the six.** Bottom-justified in a 48-pin socket, a
32-pin DIP puts its pin 32 on `Z40` and a 40-pin DIP puts its pin 40 on `Z44` --
VCC in both cases -- while chip pin N/2, commonly GND, always lands on `Z24`.
That would make three of the six the always-connected supply positions. It does
not explain `Z10`, `Z23` or `Z32`, and 28-pin parts would predict `Z38`, which
is isolated. Offered as something to test, not as a conclusion.

### ISP header

`ISP:J02` confirmed: header position 2 decoded as `J02`, 32 frames, zero
framing errors. First piece of radiomanV's pinout this project has verified
itself rather than inherited.

### What is still unknown

The 42 isolated positions need the driver network enabled, which means
characterising the rail shift register — bit order and OE/LE polarity, both
still unknown. That job is now much more tractable: this tool is a 16-channel
parallel observer that can watch which positions come alive as patterns are
shifted in, instead of one meter probe at a time.

## How it works

`t76_beacon` drives all 48 ZIF pins at once, each transmitting its own name
(`Z12\r\n`) at 115200 8N1. The property that makes this cheap: **every pin
transmits in lockstep from one shared sequencer.** So this is not 26
independent UART receivers — it is one sampler plus a software decode per
channel.

- One PIO instruction (`in pins, 32`) samples all GPIOs at 1.152 MSa/s
- DMA lands 16384 samples (14.2 ms, ~32 beacon frames) in RAM
- Each channel is decoded in software at Q8 fractional bit positions

The decode uses the beacon's **real** baud rate, not its nominal one: its
divider is `20e6 / 115200 = 173.6` rounded to 174, so it actually transmits at
114942.5 baud.

## Wiring

**The T76 socket is a 48-pin ZIF — 24 positions per side.** The Pico has 26
usable header GPIOs, so **one physical row maps per pass**, with two to spare.

### Do not insert the Pico directly

A Pico is **0.7 in** between pin rows; a wide DIP socket is **0.6 in**. It will
not seat, and forcing it risks the socket. Use jumper wires — which you want
anyway, because each line needs a series resistor.

### Every line gets a 10 kΩ series resistor

Not optional. The ZIF rails are FPGA-switched and **their polarity is still
unverified** — the census drives them to `0` as a *guess* at "off". 10 kΩ keeps
a rail misfire at ~1 mA into the RP2040's clamp instead of a dead board, and is
transparent at beacon baud (a 500 ns edge against an 8.7 µs bit).

Measured on the bench: the socket is a **3.3 V I/O domain** (VCCIO 3.456 V), so
the logic levels themselves are safe. The resistors are there for the rails.

### Ground

**From the Pico's USB, shared with the T76 through the host. Never from a
socket pin.** On this board every exposed "GND" is a switch output — the ISP
header's ground pins are FPGA-switched, and the socket's ground rail runs
through the shift register. A socket pin can stop being ground at any moment.

### Pass layout

| Pass | `SOCKET_BASE` | Socket positions | Channels |
|---|---|---|---|
| A | `1` | 1–24 (one row) | `GP0`–`GP22`, `GP26` |
| B | `25` | 25–48 (other row) | same |

`CHANNELS[i]` carries socket position `SOCKET_BASE + i`. Wire sequentially and
the labels take care of themselves.

**Orientation checks itself.** If the end you called "pin 1" was the wrong end,
`ZIF01` shows up at position 24 and the whole map reads reversed — visible,
rather than silently mirrored. So pick an end, be consistent, and let the
output confirm it.

## Running

The Rusty Probe Lite on the Pico's SWD header; `.cargo/config.toml` pins
probe-rs to it by VID:PID so the U50C FTDI is never touched.

```sh
cargo run --release      # flash + stream RTT
```

`DEFMT_LOG` is set in `.cargo/config.toml`. It is a **compile-time** filter —
unset, no logging is compiled in at all and the board looks dead. That cost a
debugging round here.

The capture repeats every 3 seconds, so wires can be moved and the map re-read
without reflashing. Nothing is driven into the socket; every channel is a
pulled-down input.

## Reading the output

```
socket  gpio  name   frames  bytes   err  duty%  minrun
  07    GP06  Z07     0032   0160  0000   089   0010
```

- **frames** — complete `Xnn\r\n` beacon frames. All channels share one
  sequencer, so a healthy capture has every connected channel within a frame or
  two of its neighbours. A channel well below its neighbours is marginal, not
  mapped.
- **err** — falling edges that looked like a start bit and did not frame.
- **minrun** — shortest run of identical samples, i.e. one bit time in samples.
  Should read ~10. A different value means the baud assumption is wrong, which
  otherwise presents as silence.

Unidentified channels say *how* they failed rather than just "none":

| Report | Means |
|---|---|
| `stuck low` | open circuit, or the pin really is grounded |
| `idle-high (no beacon)` | connected, but nothing is transmitting |
| `bytes but no name` | framing works, content doesn't — wrong baud |
| `activity, no framing` | signal present, not a clean UART |

## Self-test

Every boot, the firmware synthesises a `Z07\r\n` capture at the exact bit
timing the field decode uses, plus an idle-high and a stuck-low channel, and
decodes them. A decoder that reports silence is indistinguishable from a quiet
line, and this project has already lost time to an instrument that looked like
dead hardware. If `selftest FAIL` appears, nothing below it is worth reading.

## Next

Once the map is confirmed, the same board can emulate a DRAM in the socket —
RP2040 PIO handles 150 ns RAS/CAS timing comfortably — giving
hardware-in-the-loop validation of `fpga/dramtest/` with no vintage part at
risk. That needs the map first, to know which socket position the controller
drives as which signal.
