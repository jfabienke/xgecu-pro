# When the tools report success over a real failure

Collected from one bench session, 2026-08-25/26, because six distinct tools did
it and the pattern is clearly not a one-off. Every case produced **plausible,
well-formatted output**. Not one announced itself as wrong.

Every single one was caught the same way: **a second measurement disagreeing
with the first.** None was caught by anything looking suspicious on its own.

## The cases

**A single-sided pull hid 42 grounded pins.** The RP2040 probe read socket
contacts with an internal pull-down. Under a pull-down, a floating contact and a
contact the board is actively grounding both read low. 42 grounded positions were
recorded as "isolated behind the driver network", and the reading was *stable and
repeatable* -- it survived pressing the board, nudging it sideways, and moving it
along the socket, because it was reporting a real fact. Just not the one it was
labelled with. Sampling under a pull-up **and** a pull-down separates them.

**A safe state that was the enabled state.** Every bitstream held the socket's
rail output enables at `0`, in a comment I wrote, believing that meant off. They
are **active low**. The ground rail had been driving the socket throughout, with
whatever pattern was left in the shift register. Changing three constants took a
placement from 2 identified positions to 16 of 16 without moving the board.

**`head` masked `iverilog`'s exit status. Twice.**

```sh
iverilog ... | head -6 && echo "COMPILES CLEAN"     # WRONG
```

`head` exits 0 whatever the compiler did. Both times this printed COMPILES CLEAN
over a real elaboration error. Redirect to a file and test the compiler's own
status.

**Two continuous drivers on one wire, compiled silently.** Copied boilerplate
left `assign vcc_oe = 1'b0` alongside the state machine's own driver. Verilog
resolves that to `X` -- on the single signal deciding whether 5 V reaches a
socket contact. `iverilog` said nothing. Caught by asserting that every rail
signal is assigned exactly once:

```sh
for sig in ser_clk ser_data vcc_le vcc_oe vpp_le vpp_oe gnd_le gnd_oe; do
  printf "  %-9s %s\n" "$sig" "$(grep -c "^assign $sig " design.v)"
done
```

**TD emitted a bitstream and exited 0 with six ERRORs.** A testbench copied into
the build directory became the top module. Trusting that exit code would have
flashed a garbage bitstream onto hardware that drives a shared bus between two
live chips. Grep the log for `ERROR`; do not trust the status.

**A truncated frame carried a valid preamble.** The receiver's capture window was
shorter than the frame, so every frame was cut -- but the fragment still began
with `55 AA 55 AA` further in, and decoded into plausible nonsense. Size the
window against the frame *period*, not the frame length.

**A stale bitstream produced a perfect-looking result.** One experiment ran
against a device still wedged from the previous run, so the new bitstream never
loaded and the capture came from the *previous* configuration still resident. It
would have been recorded as a valid data point for the wrong condition.

## What actually works

- **Two measurements, not one.** Every case above was caught by a disagreement.
  A single reading has nothing to be wrong against.
- **A control in the same reading.** `Z23` staying high was only meaningful
  because `Z20` and `Z21` went low in the same breath. Absence of signal needs
  something present beside it.
- **Gate on preconditions, not just results.** Confirm the device enumerated and
  the load reported success *before* trusting any capture. A failed load followed
  by a successful-looking capture is indistinguishable from a real result.
- **Never `| head` a compiler.** Redirect and test the real exit status.
- **Count assignments** to anything that drives a rail or a shared bus.
- **A gap in coverage looks exactly like a gap in the hardware.** `Z22` read as
  non-switchable purely because it sat under the probe's own ground pin in every
  placement that reached it. Track what was *tested*, separately from what was
  *found*.
