#!/usr/bin/env python3
"""
Decode T76 static-census frames.

  ./decode_census.py /dev/tty.usbserial-XXXX      # live, 115200 8N1
  ./decode_census.py capture.bin                  # a saved byte stream
  ./decode_census.py capture.bin --raw            # per-pin table, no grouping

Frame layout (little-endian bit order within each byte, pin i at bit i%8 of
byte i//8):

    55 AA 55 AA   preamble
    01            version
    NN            pin count
    <NBYTES>      snapshot  - level when the frame was latched
    <NBYTES>      ever_low  - went low at least once during the window
    <NBYTES>      ever_high - went high at least once during the window
    CRC16         CRC-16/CCITT-FALSE over version .. last payload byte

Each pin therefore reports one of: tied low, tied high, active (both seen), or
floating/unknown (neither seen, which should not happen and is flagged).
"""
import json, sys
from pathlib import Path

PRE = bytes([0x55, 0xAA, 0x55, 0xAA])
HERE = Path(__file__).resolve().parent


def crc16(data):
    c = 0xFFFF
    for d in data:
        c ^= d << 8
        for _ in range(8):
            c = ((c << 1) ^ 0x1021) & 0xFFFF if c & 0x8000 else (c << 1) & 0xFFFF
    return c


def frames(stream, nbytes):
    """Yield (snapshot, ever_low, ever_high) bit-lists from a byte stream."""
    flen = 6 + 3 * nbytes + 2
    buf = bytearray()
    for chunk in stream:
        buf.extend(chunk)
        while True:
            i = buf.find(PRE)
            if i < 0:
                del buf[:max(0, len(buf) - 3)]
                break
            if len(buf) - i < flen:
                del buf[:i]
                break
            f = bytes(buf[i:i + flen])
            del buf[:i + flen]
            if f[4] != 0x01:
                continue
            if crc16(f[4:6 + 3 * nbytes]) != (f[flen - 2] << 8 | f[flen - 1]):
                print("  (frame with bad CRC skipped)", file=sys.stderr)
                continue
            def bits(off):
                return [(f[6 + off + j // 8] >> (j % 8)) & 1 for j in range(f[5])]
            yield bits(0), bits(nbytes), bits(2 * nbytes)


# Header pins 27 and 28 are switched to ground by this design (see
# census_isp_power.vh), so they read low because of us, not because of the
# board. Flag them rather than let the reading be taken at face value.
SELF_GROUNDED = {"j_11", "j_21", "j_26", "j_27", "j_28"}


def classify(lo, hi):
    if lo and hi:
        return "ACTIVE"
    if hi:
        return "tied high"
    if lo:
        return "tied low"
    return "?? never sampled"


def group_of(p):
    n, net = p["name"], p["net"]
    if net.startswith("Cpu:"):
        return "MCU link (HSPI / BUS candidate)"
    if n.startswith("zif_"):
        return "ZIF socket"
    if n.startswith("j_"):
        return "ISP header"
    return "straps, JTAG, unidentified"


def main():
    args = [a for a in sys.argv[1:] if not a.startswith("-")]
    raw = "--raw" in sys.argv
    if not args:
        print(__doc__)
        return 1
    pm = json.loads((HERE / "pinmap.json").read_text())
    pins, nbytes = pm["pins"], pm["nbytes"]

    src = args[0]
    if src.startswith("/dev/"):
        try:
            import serial  # pyserial
        except ImportError:
            print("pyserial needed for live capture:  uv pip install pyserial", file=sys.stderr)
            return 2
        port = serial.Serial(src, 115200, timeout=1)
        stream = iter(lambda: port.read(256), b"")
    else:
        stream = iter([Path(src).read_bytes()])

    for snap, lo, hi in frames(stream, nbytes):
        print("=" * 72)
        if raw:
            for p in pins:
                i = p["index"]
                print("  [%3d] %-14s %-5s %-14s %-9s snapshot=%d"
                      % (i, p["name"], p["ball"], p["net"], classify(lo[i], hi[i]), snap[i]))
        else:
            groups = {}
            for p in pins:
                groups.setdefault(group_of(p), []).append(p)
            for g in ("MCU link (HSPI / BUS candidate)", "straps, JTAG, unidentified",
                      "ZIF socket", "ISP header"):
                if g not in groups:
                    continue
                print("\n%s" % g)
                act = [p for p in groups[g] if lo[p["index"]] and hi[p["index"]]]
                for state in ("ACTIVE", "tied high", "tied low", "?? never sampled"):
                    sel = [p for p in groups[g]
                           if classify(lo[p["index"]], hi[p["index"]]) == state]
                    if not sel:
                        continue
                    names = ", ".join(
                        "%s(%s)%s" % (p["signal"] if p["signal"] != p["net"] else p["name"],
                                      p["ball"],
                                      " [grounded by us]" if p["name"] in SELF_GROUNDED else "")
                        for p in sel)
                    print("  %-16s %2d  %s" % (state, len(sel), names))
                if g.startswith("MCU link"):
                    hd = [p for p in act if p["signal"].startswith("HD")]
                    print("  -> %d of 24 wired HD lines are active" % len(hd))
        break   # one frame is a census; use --raw or re-run for more
    return 0


if __name__ == "__main__":
    sys.exit(main())
