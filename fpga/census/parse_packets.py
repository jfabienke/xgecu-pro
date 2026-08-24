#!/usr/bin/env python3
"""
Parse captured HSPI words into packets and decode the header fields.

Framing is established, not guessed: a CRC-16 search matched two independent
packets on one parameter set, and the header decodes per CH569 datasheet 10.2.2
with TSQN incrementing across consecutive packets.

    packet = 2 header words + 4 payload words + 1 CRC word   (16-bit words)
    header = TLL2B[31:30] | TSQN[29:26] | USDF[25:0], low half transmitted first
    CRC-16 poly 0x8005, init 0xFFFF, refin, refout, xorout 0xFFFF, LE bytes
             over the two header words and the four payload words
"""
import json, sys, importlib.util
from pathlib import Path

HERE = Path(__file__).resolve().parent
PKT_WORDS = 7
HDR_WORDS = 2
PAY_WORDS = 4


def crc16(data):
    def rev(x, n): return int(format(x, "0%db" % n)[::-1], 2)
    c = 0xFFFF
    for b in data:
        c ^= rev(b, 8) << 8
        for _ in range(8):
            c = ((c << 1) ^ 0x8005) & 0xFFFF if c & 0x8000 else (c << 1) & 0xFFFF
    return rev(c, 16) ^ 0xFFFF


def packets(words):
    """Split a word stream into packets, checking each CRC."""
    for base in range(0, len(words) - PKT_WORDS + 1, PKT_WORDS):
        w = [x & 0xFFFF for x in words[base:base + PKT_WORDS]]
        body = bytearray()
        for x in w[:HDR_WORDS + PAY_WORDS]:
            body += bytes([x & 0xFF, x >> 8])
        hdr = (w[1] << 16) | w[0]
        yield {
            "index":   base // PKT_WORDS,
            "header":  hdr,
            "tll2b":   (hdr >> 30) & 0x3,
            "tsqn":    (hdr >> 26) & 0xF,
            "usdf":    hdr & 0x03FFFFFF,
            "payload": w[HDR_WORDS:HDR_WORDS + PAY_WORDS],
            "crc":     w[-1],
            "crc_ok":  crc16(bytes(body)) == w[-1],
        }


def words_from_capture(path):
    spec = importlib.util.spec_from_file_location("d", HERE / "decode_census.py")
    d = importlib.util.module_from_spec(spec); spec.loader.exec_module(d)
    pm = json.loads((HERE / "pinmap.json").read_text())
    best = None
    for *_ , st in d.frames(iter([Path(path).read_bytes()]),
                            pm["nbytes"], pm.get("ndetail", 32)):
        if st["bursts"]:
            best = st["words"]
    return best or []


def main():
    if len(sys.argv) < 2:
        print(__doc__); return 1
    words = words_from_capture(sys.argv[1])
    if not words:
        print("  no burst captured in %s" % sys.argv[1]); return 1
    print("  %d words captured -> %d whole packets" % (len(words), len(words)//PKT_WORDS))
    print("  %-3s %-10s %-5s %-5s %-10s %-22s %-6s" %
          ("#", "header", "TLL2B", "TSQN", "USDF", "payload", "CRC"))
    for p in packets(words):
        print("  %-3d 0x%08X %-5d %-5d 0x%08X %-22s %04X %s" %
              (p["index"], p["header"], p["tll2b"], p["tsqn"], p["usdf"],
               " ".join("%04X" % x for x in p["payload"]), p["crc"],
               "ok" if p["crc_ok"] else "BAD"))
    return 0


if __name__ == "__main__":
    sys.exit(main())
