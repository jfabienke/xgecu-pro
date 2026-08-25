#!/usr/bin/env python3
"""
Bring-up beacon: drive EVERY candidate pin with its own identity over UART.

The census transmits on a single pin, which makes finding that pin a guessing
game -- and guessing wrong looks identical to the design being broken. This
instead drives every ISP header pin and every ZIF pin simultaneously, each one
sending its OWN name ("J05\\r\\n", "Z12\\r\\n", ...). Touch a probe anywhere and
the text says which pin you touched, so one connection resolves both "is the
FPGA running" and "what is this pin called".

All pins transmit in lockstep from one shared sequencer, so the per-pin cost is
a handful of LUTs: only the two digit characters differ between instances.
"""
import re, zipfile
from pathlib import Path
from xml.etree import ElementTree as ET

HW = Path(__file__).resolve().parents[2] / "docs" / "hardware"
NS = {"t": "urn:oasis:names:tc:opendocument:xmlns:table:1.0",
      "x": "urn:oasis:names:tc:opendocument:xmlns:text:1.0"}
CLK_BALL = "E10"
# Keep these two as ground so the probe has a reference (same as the census).
# Switch every available ground so the probe's ground lead can land almost
# anywhere on the header. A pin that is grounded must NOT also be driven by the
# FPGA -- that would be contention against the ground switch -- so these five
# are excluded from the transmitting set below.
GND_KEEP = {"ISP:GND11": "j_gnd_11", "ISP:GND21": "j_gnd_21",
            "ISP:GND26": "j_gnd_26", "ISP:GND27": "j_gnd_27",
            "ISP:GND28": "j_gnd_28"}
GND_OFF = {}
GROUNDED_POSITIONS = {11, 21, 26, 27, 28}
PWR_OFF = {"ISP:VCC4": "j_vcc_04", "ISP:VCC20": "j_vcc_20", "ISP:VCC22": "j_vcc_22",
           "ISP:VCC24": "j_vcc_24", "ISP:VPP24": "j_vpp_24", "ISP:VPP26": "j_vpp_26"}
RAIL = {"T:SCLK": "ser_clk", "T:SDAT": "ser_data", "T:LE_VPP": "vpp_le",
        "T:LE_VCC": "vcc_le", "T:LE_GND": "gnd_le", "T:OE_VPP": "vpp_oe",
        "T:OE_VCC": "vcc_oe", "T:OE_GND": "gnd_oe"}
DEDICATED = {"T2", "C12", "A15", "C14", "E14"}


# --- corrections to the third-party pinout ------------------------------------
# docs/hardware/fpga_t76_pinout.ods is radiomanV's board tracing, byte-identical
# to his copy, and is deliberately left untouched: its provenance is worth more
# than the convenience of editing it. Corrections we have MEASURED live here
# instead, so the vendor data stays pristine and every deviation is auditable.
#
# M4/M5: measured 2026-08-25 with an RP2040 reading the beacon on the ISP header,
# from BOTH rows independently. Physical header pin 7 carried the name the pinout
# gives to M4 ("J08") and pin 8 carried M5's ("J07"), so the two labels are
# transposed. The .ods even lists them out of sequence -- J06, J08, J07 -- which
# is the same mistake showing through in the source. Every other one of the 19
# signal positions matched the pinout exactly, and six controls (two positions
# with no net, four switched grounds) reported nothing as predicted.
PINOUT_FIXES = {
    "M4": "ISP:J07",   # .ods says ISP:J08
    "M5": "ISP:J08",   # .ods says ISP:J07
}


def ball_map():
    x = ET.fromstring(zipfile.ZipFile(HW / "fpga_t76_pinout.ods").read("content.xml"))
    grid = []
    for tb in x.iter("{%s}table" % NS["t"]):
        for r in tb.iter("{%s}table-row" % NS["t"]):
            cs = []
            for c in r.iter("{%s}table-cell" % NS["t"]):
                txt = "".join(n.text or "" for n in c.iter("{%s}p" % NS["x"]))
                rep = int(c.get("{%s}number-columns-repeated" % NS["t"], 1))
                cs.extend([txt.strip()] * min(rep, 32))
            grid.append(cs)
    cols = [h for h in grid[0][1:] if h.isdigit()]
    m = {}
    for row in grid[1:]:
        if not row or not re.match(r"^[A-Y]$", row[0]):
            continue
        for i, v in enumerate(row[1:len(cols) + 1]):
            if v:
                m["%s%s" % (row[0], cols[i])] = v
    m.update(PINOUT_FIXES)
    return m


def main():
    balls = ball_map()
    here = Path(__file__).resolve().parent
    tx = []   # (verilog_name, ball, group_char, number)
    for ball, net in sorted(balls.items()):
        if ball == CLK_BALL or ball in DEDICATED:
            continue
        u = net.upper()
        if u.startswith("ISP:J"):
            n = int(re.sub(r"[^0-9]", "", net))
            if n in GROUNDED_POSITIONS:   # grounded, must not be driven
                continue
            tx.append(("j_%02d" % n, ball, "J", n))
        elif u.startswith("ZIF"):
            n = int(re.sub(r"[^0-9]", "", net))
            tx.append(("zif_%02d" % n, ball, "Z", n))
    tx.sort(key=lambda e: (e[2], e[3]))

    ports = ["    input  wire i_clock_20M,"]
    for v in list(GND_KEEP.values()) + list(GND_OFF.values()) + list(PWR_OFF.values()) + list(RAIL.values()):
        ports.append("    output wire %s," % v)
    for i, (v, b, g, n) in enumerate(tx):
        ports.append("    output wire %-10s%s  // %-5s %s%02d" % (v, "" if i == len(tx) - 1 else ",", b, g, n))
    (here / "beacon_ports.vh").write_text("\n".join(ports) + "\n")

    body = ["// Generated by gen_beacon.py -- do not edit.",
            "// Each pin sends its own name so one probe touch identifies it."]
    for v, b, g, n in tx:
        body.append("assign %-10s = tx_bit_for(8'h%02X, 8'h%02X, 8'h%02X, byte_idx, bit_idx);"
                    % (v, ord(g), ord(str(n // 10)), ord(str(n % 10))))
    (here / "beacon_drive.vh").write_text("\n".join(body) + "\n")

    IN = "{ LOCATION = %s; IOSTANDARD = LVCMOS33; PULLTYPE = NONE; PCICLAMP = ON; }"
    OUT = "{ LOCATION = %s; IOSTANDARD = LVCMOS33; DRIVESTRENGTH = 8; PULLTYPE = NONE; }"
    a = ["# T76 bring-up beacon -- generated, do not edit.",
         "set_pin_assignment\t{ i_clock_20M }\t%s" % (IN % CLK_BALL)]
    for net, v in list(GND_KEEP.items()) + list(GND_OFF.items()) + list(PWR_OFF.items()) + list(RAIL.items()):
        ball = next((bb for bb, x in balls.items() if x == net), None)
        if ball:
            a.append("set_pin_assignment\t{ %s }\t%s" % (v, OUT % ball))
    for v, b, g, n in tx:
        a.append("set_pin_assignment\t{ %s }\t%s" % (v, OUT % b))
    (here / "t76_beacon.adc").write_text("\n".join(a) + "\n")
    (here / "t76_beacon.sdc").write_text(
        "create_clock -name clk_20 -period 50.0 [get_ports i_clock_20M]\n")

    print("  %d transmitting pins (%d ISP, %d ZIF)"
          % (len(tx), sum(1 for e in tx if e[2] == "J"), sum(1 for e in tx if e[2] == "Z")))
    print("  wrote beacon_ports.vh, beacon_drive.vh, t76_beacon.adc, t76_beacon.sdc")


if __name__ == "__main__":
    main()
