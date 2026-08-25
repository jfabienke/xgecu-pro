# hspi-probe — answer the MCU, and watch what it does about it

An FPGA design that **both answers HSPI packets and captures them**. Neither
half is new; the combination is. The answering endpoint was built in `ef467fc`
and the working capture is the one restored by reverting to `a93f729` — and that
revert removed the answering code, so the two have never existed together.

**Status: running on hardware, baseline captured.**

```
   RESPONSE_MODE 0 (counter), read under MINIPRO_KEEP_BITSTREAM

   12 complete frames, 1 distinct state -- stable
   capwords = 161   bursts = 23
   0037 c800 0008 0000 0000 0000 1518 | 003a cc00 0000 0000 0000 0000 a0c6
```

The rig reproduces two prior measurements from separate sessions: `ef467fc`
recorded "23 bursts in, 161 words" and gets exactly that, and at 7 words per
packet the USDF values `0x37` then `0x3a` are the head of the failing tail the
census recorded as `0x37 3A 32 33 04`. So mode 0 lands on the **failing** path,
and that is the reference every other mode is compared against.

Two things had to be fixed before a frame could be read at all, both of which
produced confident wrong output first:

- **The Pico's window was shorter than the frame.** 14.2 ms against a 20.4 ms
  frame truncated every one, and a truncated frame still carries a valid-looking
  preamble -- it decodes into plausible nonsense rather than announcing itself
  as broken. The window is now 28.4 ms.
- **The frame period was longer than the window.** At `FRAME_GAP` = 5,000,000
  (250 ms) the window landed at a random point in the cycle and caught a whole
  frame roughly one time in nine. `FRAME_GAP` is now 100,000, making the period
  25.4 ms -- shorter than the window, so any window contains a whole frame.

`tb_txrx.v` passes every check, including the two that matter for the board:

```
   DUT raised HRACT
   DUT raised HRVLD and is transmitting
   words out: 0000 0000 | 0000 0001 0002 0003 0004 0005 | crc f34e
   transmitter: all checks passed
```

The safety checks are "NEVER drove HD while the MCU was driving" and "released
HD immediately when HTVLD went high". HD is a shared bus between two live chips;
contention on it is the one failure here that damages hardware, so it is
asserted rather than reasoned about.

## Why this exists

A USB return path for FPGA-generated data would make this programmer a general
instrument rather than a chip programmer with a hobby. `ef467fc` established the
transport is symmetric:

> Measured during a read: 23 bursts in, 161 words, 21 packets out, and 21 HTACK
> rising edges. The MCU acknowledged every packet we sent.

and that the wall is one layer up:

> The MCU acknowledges our packets in hardware and discards them in firmware.

The firmware route to the vendor's protocol is closed — the update image is
properly encrypted and the key is inside the chip (`docs/t76-firmware-container.md`).
So the remaining route is protocol RE from the bench.

## Why the census README's plan does not work

It proposes capturing later `SKIP` windows to find where a read diverges from an
erase. But packets 0-22 are already known identical, and **both operations abort
inside that range**. A read never reaches its data phase, so there is nothing
further out to capture. Widening the window captures nothing.

## Result: the MCU does not read our payload at all

Three response modes, each a separate build and bench run:

```
   mode 0  counter   12 frames, 1 distinct   capwords=161 bursts=23   USDF 0x37 0x3a
   mode 1  zeros     10 frames, 1 distinct   capwords=161 bursts=23   USDF 0x37 0x3a
   mode 3  echo      12 frames, 1 distinct   capwords=161 bursts=23   USDF 0x37 0x3a
```

**Byte-identical.** Same word count, same burst count, same USDF sequence, same
payload, each stable across 10-12 frames so none is noise.

Those are three distinct classes of response -- arbitrary, constant, and
content-derived. Echo was the one that could have mattered: if any part of the
exchange were a challenge the FPGA is meant to reflect, echo is the only mode
that satisfies it. It changed nothing.

So `ef467fc`'s suspicion is now measured rather than inferred:

> The MCU acknowledges our packets in hardware and discards them in firmware.

It discards them **without inspecting their contents**. Well-formed packets --
correct header, sequence number, valid CRC -- are necessary and nowhere near
sufficient.

### What that narrows

The question is no longer *"what bytes should we send?"* It is *"how do we get
the firmware into a state where it reads FPGA data at all?"* Payload content is
eliminated as a variable, which is a smaller search space than the one we
started with, even though the result is negative.

It also means the remaining route is genuinely the vendor protocol -- the
sequence and state that convince the firmware a real operation is underway --
and not something reachable by varying what we answer with.

## The method that got here

Stop observing and start perturbing. The success/failure signal already exists
in the census notes:

```
   failing erase tail      5 packets over registers  0x37 3A 32 33 04
   succeeding erase tail   3 packets over registers  0x01 24 27
```

The tail says which path the MCU took. So:

1. Answer with a chosen payload (`RESPONSE_MODE`)
2. Capture the MCU's packets in the same run
3. Read the tail registers out
4. Change the response, repeat

That converts "guess the vendor protocol" into **"find one input that changes
the output"** — a far smaller question, and one with a gradient to follow.

`RESPONSE_MODE` is a single localparam:

| mode | payload |
|---|---|
| 0 | free-running counter (as `ef467fc`) |
| 1 | all zeros |
| 2 | all ones |
| 3 | echo the MCU's most recent word |

Echo is the interesting one. If any part of the protocol is a challenge the FPGA
must reflect or transform, a plain echo is the cheapest probe for it.

## Safety

The bus interlock is inherited from `ef467fc` and is the one thing here that
could damage the board:

```verilog
wire hd_drive = (tx_state == 3'd2) && !HTVLD;
```

HD is driven only in the data phase **and** only while the MCU is not asserting
HTVLD — the raw pin, not a synchronised copy, so release is immediate if the MCU
takes the bus back. The transmitter also times out rather than hanging if HTACK
never arrives.

## Before it runs

- Adapt the census testbench and simulate. The census work has a standing lesson
  here: a continuous assignment calling a function froze every output at its
  time-zero value, which simulates as a dead line and on hardware looks exactly
  like a broken board.
- Check every rail and control signal is assigned exactly once. The VCC probe
  shipped with two continuous drivers on `vcc_oe` and `iverilog` said nothing.

## What the return path is worth

```
   HRCLK 20 MHz (this design)     34 MB/s after framing
   HRCLK 80 MHz (from HTCLK)     135 MB/s
   USB High Speed                ~30 MB/s real bulk     <-- binding constraint
```

**The link is not the bottleneck even at 20 MHz.** HRCLK as driven already
exceeds what USB High Speed carries past the MCU, so clocking the transmitter
faster buys nothing. That is ~2,600x the 115200 UART and ~300x a 1 Mbaud one.

But against what actually needs it:

| workload | UART 115k2 | USB HS |
|---|--:|--:|
| PAL/GAL typical (16V8) | 0.1 s | 0.00 s |
| PAL/GAL worst (22V10) | 227.6 s | 0.09 s |
| DRAM march, failure list | 0.7 s | 0.00 s |
| 48-channel capture @10 MSa/s | 5208 s | 2.00 s |

Only the last genuinely wants it -- and USB HS still cannot stream it in real
time, since 10 MSa/s across 48 channels is 60 MB/s against a 30 MB/s pipe. It
would need on-FPGA buffering either way.

So the return path is worth a great deal on paper and little in practice for
everything scoped so far. The CH569 supports SuperSpeed at 5 Gbit/s, which would
change that, but it is broken on this host and forced to High Speed by the USB-2
cable that makes bulk transfers work at all.

## Operating cost, stated plainly

Each variant is a TD build, and **each capture run wedges the T76 and needs a
physical replug**. This is dozens of bench cycles with a human in the loop,
across sessions. It is worth doing when something needs the bandwidth — the DRAM
tester would — and it is *not* worth doing for the PAL/GAL tool, where the UART
already gives 89 ms typical and 356 ms worst case with ON-set encoding.
