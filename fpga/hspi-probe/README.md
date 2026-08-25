# hspi-probe — answer the MCU, and watch what it does about it

An FPGA design that **both answers HSPI packets and captures them**. Neither
half is new; the combination is. The answering endpoint was built in `ef467fc`
and the working capture is the one restored by reverting to `a93f729` — and that
revert removed the answering code, so the two have never existed together.

**Status: compiles. Not simulated against a testbench, not synthesised, never
run.** Everything below is the plan and the reasoning, not a result.

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

## What replaces it

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

## Operating cost, stated plainly

Each variant is a TD build, and **each capture run wedges the T76 and needs a
physical replug**. This is dozens of bench cycles with a human in the loop,
across sessions. It is worth doing when something needs the bandwidth — the DRAM
tester would — and it is *not* worth doing for the PAL/GAL tool, where the UART
already gives 89 ms typical and 356 ms worst case with ON-set encoding.
