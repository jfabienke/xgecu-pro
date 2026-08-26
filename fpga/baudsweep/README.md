# baudsweep — how fast the ISP header actually goes

The readback rate is the binding constraint on every tool built on this header:
a PAL/GAL sweep resolves in milliseconds and then spends seconds getting the
answer out. 115200 was inherited from the beacon and never questioned.

One bitstream, six rates, ~2 s each, each transmitting its own step index as
`Bnn\r\n` so the receiver identifies the rate from content rather than timing.
Every divisor is an exact integer at 20 MHz, so a decode failure is the path
failing rather than the transmitter drifting.

## Result

```
   step 3    909,091 baud   measured   919,117    34 samp/bit   12 frames    0 err
   step 4  1,818,182 baud   measured 1,838,235    17 samp/bit   12 frames    0 err
   step 5  4,000,000 baud   measured 4,464,285     7 samp/bit    0 frames  289 err
```

**1.84 Mbaud decodes cleanly — 16x the rate we had been using.** Twelve frames,
zero framing errors, and the measured rate within 1% of the divisor.

What that does to the PAL/GAL tool:

| case | sweep | 115k2 | 1.84M | +ON-set 25% |
|---|--:|--:|--:|--:|
| GAL16V8 typical | 410 us | 89 ms | 6 ms | 1 ms |
| GAL16V8 worst | 26 ms | 1.4 s | 89 ms | 22 ms |
| GAL20V8 worst | 419 ms | 22.8 s | 1.4 s | 357 ms |
| GAL22V10 worst | 839 ms | 22.8 s | 1.4 s | 357 ms |

The worst case in the whole device matrix drops from 22.8 s to 1.4 s, and to
357 ms with ON-set encoding. Every device becomes interactive.

## Two things this does NOT establish

**The 4 Mbaud failure is not attributable to the path.** At 31.25 MSa/s that is
7.8 samples per bit, where sampling at bit centres is marginal. The receiver is
as likely to be at fault as the T76's driver network. The honest statement is
"at or above 1.84 Mbaud", not "1.84 and no further".

**Steps 0-2 were not measured.** The probe's run-length histogram is 64 buckets
and a 115200 bit is 271 samples at this rate, so it overflows and reports "no
transitions". Those lines are a bug in the instrument, not a failure of the
link. Widening the histogram, or sampling slower for the slow steps, would fix
it -- and 115200 is already known to work, so nothing hangs on it.

## What it retires

The argument that a USB return path is worth building *because the UART is
slow*. The UART was not slow; it was slow by assumption, at a rate nobody had
questioned. The remaining reason to want USB is that it needs no jumper to an
internal header -- which is a real argument, and now the only one.
