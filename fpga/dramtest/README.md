# DRAM controller and march tester for the T76's FPGA

Milestone 1 of turning the programmer into a memory tester: a controller for
single-rail asynchronous DRAM, plus a March C- test engine, verified in
simulation against a behavioural DRAM model.

**Status: simulation-verified, not yet on hardware.** The controller is correct
against a model with fault injection. What it does not have yet is a socket pin
mapping or rail control, and both of those are the parts that can damage a chip.

## What it targets

The 64 Kbit generation onward, single +5 V rail: `4164`, `41256`, `4416`,
`4464`, `511000`, `44256`, `514400`, and 30-pin SIMMs through a passive adapter.

Earlier three-rail parts -- the `4116` above all -- are permanently out of reach.
The T76's voltage field is a VPP/VCC code with no negative-rail encoding, so no
bitstream can produce the -5 V they need. That is a hardware limit, not a
software one.

## Design

Timings are parameters **in clock cycles**, not nanoseconds, so the same design
runs from `CLK_20` (50 ns) or from `HTCLK` (~12.5 ns, free-running) without
hand-editing. Defaults are cycles at 20 MHz for a 150 ns part with a cycle of
margin on each edge.

| Parameter | Default | Meaning |
|---|--:|---|
| `T_RP` | 3 | RAS precharge, >= 100 ns |
| `T_RAS` | 4 | RAS pulse width, >= 150 ns |
| `T_RCD` | 2 | RAS-to-CAS delay |
| `T_CAS` | 2 | CAS pulse width, >= 75 ns |
| `T_REFI` | 300 | refresh interval, ~15 us at 20 MHz |

**Refresh is distributed, not burst.** The row counter advances on its own
interval and preempts the test engine *between* accesses, never inside one. A
DRAM is only meaningfully under test while it is being refreshed correctly; a
tester that loses rows to neglect reports faults that are its own.

**The test is March C-**, reduced to the elements a socket tester can act on:
write 0 everywhere, then read-0-write-1 ascending, read-1-write-0 descending,
then a final read of 0. That catches stuck-at, transition and coupling faults.
The first failing address is latched and reported.

## Verification

`tb_dram.v` runs a behavioural async-DRAM model and checks **two** properties:

1. a good part passes with **zero** errors, and
2. an injected stuck-at cell is **caught, at the right address**.

The second matters as much as the first. A tester that never reports a fault is
worthless, and a green run against a perfect model proves only half of it.

```sh
iverilog -B /opt/zb/lib/ivl -g2005 -o /tmp/tbd.vvp tb_dram.v dram_ctrl.v
vvp /tmp/tbd.vvp
```

### Three bugs the testbench found

All three were invisible without fault injection or without isolating refresh:

- **The read half was writing.** A read-modify element sets both `do_read` and
  `do_write`; the access engine asserted `WE` whenever `do_write` was set, so
  every access was a write. Phase 2 still "passed", because it expected 0 and a
  broken read also returns 0 -- the failure only became visible when a later
  phase expected 1.
- **Refresh raised `acc_done`.** A refresh shares the precharge state with a
  real access, so it signalled completion; the march engine then checked stale
  read data and advanced past an address it never accessed. Isolating this took
  one run with refresh disabled, which passed cleanly.
- **The access request was a pulse.** `acc_start` was single-cycle, and a
  refresh taking priority that cycle swallowed it, hanging the engine. The
  spurious `acc_done` above had been masking it, so fixing one exposed the
  other. It is now a request/acknowledge handshake.

## Not done yet, and both are the dangerous parts

- **Socket pin mapping.** Which ZIF positions a 16-pin DIP lands on has not been
  established. Getting it wrong applies +5 V to the wrong pin.
- **Rail control and polarity.** The census drives `T:OE_*`/`T:LE_*` to a
  *guessed* off state and has only ever run with an empty socket. A tester must
  deliberately energise specific pins, so the shift-register protocol and its
  polarity have to be established -- with a meter, on an empty socket -- before
  any part goes in.

Nothing here should meet a real chip until both are settled.
