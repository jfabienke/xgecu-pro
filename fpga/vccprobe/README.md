# vccprobe — what the FPGA does *not* control

Two questions, one experiment, because they turned out to be the same one:
is `vcc_oe` active low like `gnd_oe`, and what are `Z23`/`Z24`?

## The FPGA does not control the VCC supply

Neither polarity of `vcc_oe` puts voltage on a socket contact. Tested with a
two-state cycle, deliberately **asymmetric** in duration so a meter alone
identifies the states with no UART jumper and no correlating of timestamps:

```
    all-ones, vcc_oe = 0   held  4 s   SHORT
    all-ones, vcc_oe = 1   held 16 s   LONG
```

`Z22` sat at a steady ~3 V through both. That is the resting pull-up level, not
a driven rail.

**Energising the socket is a transaction, not an FPGA state.** `Programmer::end`
is documented as "de-energize the socket", so `BEGIN_TRANS` is what powers it,
and no raw bitstream upload issues one. Grounding worked throughout this project
precisely because grounding needs no supply — it is switches to ground.

Confirmed with a real chip read on an empty socket, watched on the scope: a
socket position sat at 0.11 V for nine polls, then jumped to **3.56 V** and held
for the rest of the operation.

## Z24 is ground for every part

Watched during an identical read, `Z24` **never moves** — 0.11 V idle, 0.14 V
throughout, while another position swung to 3.56 V in the same operation.

There is a reason it would be. For any DIP bottom-justified in a 48-pin socket,
chip pin `N/2` lands on `Z24`:

```
   24-pin -> pin 12      32-pin -> pin 16
   28-pin -> pin 14      40-pin -> pin 20
```

On essentially every standard DIP memory device, pin `N/2` is ground. `Z24` is
the one socket position that is ground for *every* supported part, which is
ample reason for it to be permanent rather than switched — and it explains why
the ground chain never reached it. It does not need to.

## What is still open

Under `railhold`, the Pico's pull probe read `Z24` as held **high**. Under a chip
algorithm, the scope reads it at **ground**. Not necessarily contradictory --
different bitstreams, and `(1,1)` on the pull probe can mean "floating with a
board pull-up" as easily as "actively driven high" -- but it does mean `Z24`'s
ground is conditional on something, and the chain bits swept in
`fpga/railsweep` are not what controls it.

**What `Z24` is:** answered. **How it gets grounded:** open.

`Z23` remains unexplained. It is not in the ground chain and not obviously a
supply position.

## A bug worth recording

The first build carried a stale `assign vcc_oe = 1'b0` over from copied
boilerplate, alongside the state machine's own driver. Two continuous drivers on
one wire resolve to `X` -- on the single signal that decides whether 5 V appears
on a socket contact.

`iverilog` compiled it without complaint. What caught it was asserting that every
rail signal is assigned exactly once:

```sh
for sig in ser_clk ser_data vcc_le vcc_oe vpp_le vpp_oe gnd_le gnd_oe; do
  printf "  %-9s %s\n" "$sig" "$(grep -c "^assign $sig " design.v)"
done
```

Worth keeping permanently for anything that drives a rail.

## Instruments

The meter and a Rigol MHO954 over VXI-11 (`~/Development/rigol`), not the Pico.
3.3 V logic has no business near a 5 V rail, and after a day in which the pull
probe, the decoder and contact timing each produced confident wrong answers, an
independent direct reading is the point rather than the fallback.

Two scope traps, both of which produced frozen or displaced numbers first time:

- **Acquisition must actually be running.** Ten polls returned values identical
  to four decimal places, which is one held value rather than ten measurements.
  `rigol mho954 run` fixed it; consecutive readings then differ, which is the
  check that it is live.
- **Ground the scope to the T76's USB-C shell.** `ISP:J01` has no net in the
  pinout and measured ~3 V below the shell -- another position on the negative
  rail. A bench scope's earthed clip on an unconnected pin happens to work, but
  it is earth doing it, not the pin.
