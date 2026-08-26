#!/usr/bin/env python3
"""
Generate ports and pin constraints for the baud sweep.

Deliberately smaller than gen_beacon: the sweep declares ONLY the rail control
lines, the ISP header's power/ground switches, the clock and one UART pin. The
48 ZIF balls are left undeclared so the FPGA cannot drive them -- a ground
switch closing on a driven pin would be an FPGA output fighting a ground
switch, which the census source warns about explicitly.
"""
import re, zipfile
from pathlib import Path
from xml.etree import ElementTree as ET

HW = Path(__file__).resolve().parents[2] / "docs" / "hardware"
NS = {"t": "urn:oasis:names:tc:opendocument:xmlns:table:1.0",
      "x": "urn:oasis:names:tc:opendocument:xmlns:text:1.0"}

CLK_BALL  = "E10"
UART_BALL = "M4"      # ISP:J07 -> physical header pin 7 (see PINOUT_FIXES)

RAIL = {"T:SCLK": "ser_clk", "T:SDAT": "ser_data",
        "T:LE_VPP": "vpp_le", "T:LE_VCC": "vcc_le", "T:LE_GND": "gnd_le",
        "T:OE_VPP": "vpp_oe", "T:OE_VCC": "vcc_oe", "T:OE_GND": "gnd_oe"}
ISP_POWER = {"ISP:GND11": "j_gnd_11", "ISP:GND21": "j_gnd_21",
             "ISP:GND26": "j_gnd_26", "ISP:GND27": "j_gnd_27",
             "ISP:GND28": "j_gnd_28",
             "ISP:VCC4": "j_vcc_04", "ISP:VCC20": "j_vcc_20",
             "ISP:VCC22": "j_vcc_22", "ISP:VCC24": "j_vcc_24",
             "ISP:VPP24": "j_vpp_24", "ISP:VPP26": "j_vpp_26"}

# Same measured correction as the other generators; see fpga/census/gen_census.py.
PINOUT_FIXES = {"M4": "ISP:J07", "M5": "ISP:J08"}


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
    outs = list(RAIL.values()) + list(ISP_POWER.values())

    ports = ["    input  wire i_clock_20M,", "    output wire uart_tx,"]
    for i, v in enumerate(outs):
        ports.append("    output wire %s%s" % (v, "" if i == len(outs) - 1 else ","))
    (here / "baudsweep_ports.vh").write_text("\n".join(ports) + "\n")

    IN  = "{ LOCATION = %s; IOSTANDARD = LVCMOS33; PULLTYPE = NONE; PCICLAMP = ON; }"
    OUT = "{ LOCATION = %s; IOSTANDARD = LVCMOS33; DRIVESTRENGTH = 8; PULLTYPE = NONE; }"
    a = ["# T76 baud sweep -- generated, do not edit.",
         "set_pin_assignment\t{ i_clock_20M }\t%s" % (IN % CLK_BALL),
         "set_pin_assignment\t{ uart_tx }\t%s" % (OUT % UART_BALL)]
    missing = []
    for net, v in list(RAIL.items()) + list(ISP_POWER.items()):
        ball = next((b for b, n in balls.items() if n == net), None)
        if ball:
            a.append("set_pin_assignment\t{ %s }\t%s" % (v, OUT % ball))
        else:
            missing.append(net)
    (here / "t76_baudsweep.adc").write_text("\n".join(a) + "\n")
    (here / "t76_baudsweep.sdc").write_text(
        "create_clock -name clk_20 -period 50.0 [get_ports i_clock_20M]\n")
    (here / "build.tcl").write_text(
        "import_device eagle_20.db -package BGA256X\n"
        "read_verilog t76_baudsweep.v\n"
        "read_adc t76_baudsweep.adc\n"
        "read_sdc t76_baudsweep.sdc\n"
        "optimize_rtl\noptimize_gate\nlegalize_phy_inst\nplace\nroute\n"
        "bitgen -bit t76_baudsweep.bit -version 0X00 "
        "-g ucode:000000000000000000000000 -info -log_file t76_baudsweep_bit.log\n"
        "exit\n")

    print("  %d driven pins (%d rail, %d ISP power) + clock + uart"
          % (len(outs), len(RAIL), len(ISP_POWER)))
    if missing:
        print("  WARNING: no ball found for: %s" % ", ".join(missing))
    print("  uart on %s = %s" % (UART_BALL, balls.get(UART_BALL, "?")))
    print("  wrote baudsweep_ports.vh, t76_baudsweep.adc/.sdc, build.tcl")


if __name__ == "__main__":
    main()
