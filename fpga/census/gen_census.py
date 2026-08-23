#!/usr/bin/env python3
"""
Generate the T76 static-census pin constraints, Verilog port list, and host
decode table from ONE source of truth: docs/hardware/fpga_t76_pinout.ods
crossed with docs/hardware/CH569W_pinout.ods.

Emits:
  census_ports.vh   - Verilog port declarations (included by t76_census.v)
  census_obs.vh     - the observed-pin concatenation vector
  t76_census.adc    - Anlogic TD pin constraints
  pinmap.json       - index -> {ball, net, signal} for decode_census.py

Everything downstream indexes pins the same way because it all comes from here.
Re-run after any pinout correction; never hand-edit the generated files.
"""
import json, re, sys, zipfile
from pathlib import Path
from xml.etree import ElementTree as ET

HW = Path(__file__).resolve().parents[2] / "docs" / "hardware"
NS = {"t": "urn:oasis:names:tc:opendocument:xmlns:table:1.0",
      "x": "urn:oasis:names:tc:opendocument:xmlns:text:1.0"}


def ods_rows(path):
    x = ET.fromstring(zipfile.ZipFile(path).read("content.xml"))
    out = []
    for tb in x.iter("{%s}table" % NS["t"]):
        for r in tb.iter("{%s}table-row" % NS["t"]):
            cs = []
            for c in r.iter("{%s}table-cell" % NS["t"]):
                txt = "".join(n.text or "" for n in c.iter("{%s}p" % NS["x"]))
                rep = int(c.get("{%s}number-columns-repeated" % NS["t"], 1))
                cs.extend([txt.strip()] * min(rep, 32))
            out.append(cs)
    return out


def ball_map():
    """ball id (e.g. 'E10') -> net name as silkscreened in the T76 pinout."""
    g = ods_rows(HW / "fpga_t76_pinout.ods")
    cols = [h for h in g[0][1:] if h.isdigit()]
    m = {}
    for row in g[1:]:
        if not row or not re.match(r"^[A-Y]$", row[0]):
            continue
        for i, val in enumerate(row[1:len(cols) + 1]):
            if val:
                m["%s%s" % (row[0], cols[i])] = val
    return m


def ch569_functions():
    """CH569W pin number -> full alternate-function string."""
    fn = {}
    for r in ods_rows(HW / "CH569W_pinout.ods"):
        for i, c in enumerate(r):
            if re.match(r"^\d{1,2}$", c) and i + 3 < len(r) and "/" in (r[i + 3] or ""):
                fn.setdefault(int(c), r[i + 3])
                break
    return fn


HSPI = re.compile(r"^(HD\d+|HTCLK|HRCLK|HTREQ|HTACK|HTVLD|HRVLD|HTRDY|HRACT)$")


def hspi_name(func):
    for tok in func.split("/"):
        if HSPI.match(tok):
            return tok
    return None


# --- pins we must NOT observe: they have a job ------------------------------
CLK_BALL = "E10"      # CLK_20, our sampling clock - deliberately independent
                      # of the bus under test, so a dead bus is still reportable
UART_BALL = "E13"     # ISP:J19, proven reachable by radiomanV's design

# Rail control is DRIVEN to a static safe state, never observed: leaving these
# floating would let the ZIF rail drivers do something undefined.  Polarity is
# not documented, so this is a best guess -- which is exactly why the socket
# must be EMPTY when running the census.  See README.
RAIL = {"T:SCLK": "ser_clk", "T:SDAT": "ser_data",
        "T:LE_VPP": "vpp_le", "T:LE_VCC": "vcc_le", "T:LE_GND": "gnd_le",
        "T:OE_VPP": "vpp_oe", "T:OE_VCC": "vcc_oe", "T:OE_GND": "gnd_oe"}

SKIP = re.compile(r"^(vcc_int|vcc_aux|adc_vdda|vio_\d|gnd|vcc|nc|-)$", re.I)


def classify(net):
    u = net.upper()
    if u.startswith("ZIF"):
        return "zif"
    if u.startswith("CPU:"):
        return "cpu"
    if u.startswith("ISP:J") and not u.startswith(("ISP:VCC", "ISP:VPP", "ISP:GND")):
        return "isp"
    if u in ("M0", "M1"):
        return "strap"
    if re.match(r"^(TCK|TDI|TDO|TMS)\(NC\)$", u):
        return "jtag"
    if "?" in net:
        return "unknown"
    return None


def main():
    balls, fns = ball_map(), ch569_functions()
    observed = []          # (verilog_name, ball, net, resolved_signal)

    for ball, net in sorted(balls.items()):
        if ball in (CLK_BALL, UART_BALL) or net in RAIL or SKIP.match(net):
            continue
        kind = classify(net)
        if kind is None:
            continue
        if kind == "cpu":
            pin = int(net.split(":")[1])
            func = fns.get(pin, "?")
            sig = hspi_name(func)
            # Fall back to the first alternate function so the one non-HSPI CPU
            # ball still gets a meaningful name (it is PA7/BD7/RXD1).
            name = sig or ("cpu_p%d" % pin)
            observed.append((name, ball, net, sig or func))
        elif kind == "zif":
            n = re.sub(r"[^0-9]", "", net)
            observed.append(("zif_%02d" % int(n), ball, net, net))
        elif kind == "isp":
            n = re.sub(r"[^0-9]", "", net)
            observed.append(("j_%02d" % int(n), ball, net, net))
        elif kind == "strap":
            observed.append((net.lower(), ball, net, net))
        elif kind == "jtag":
            observed.append((net.split("(")[0].lower(), ball, net, net))
        elif kind == "unknown":
            observed.append(("unknown_%s" % ball.lower(), ball, net, net))

    # stable, human-meaningful order: cpu bus, then straps/jtag/unknown, then zif, then isp
    def rank(e):
        n = e[0]
        if re.match(r"^(HD\d+|HT|HR|cpu_p)", n): return (0, n)
        if n in ("m0", "m1") or n in ("tck", "tdi", "tdo", "tms") or n.startswith("unknown"): return (1, n)
        if n.startswith("zif_"): return (2, n)
        return (3, n)
    observed.sort(key=rank)

    n = len(observed)
    nbytes = (n + 7) // 8
    here = Path(__file__).resolve().parent

    # ---- Verilog ports -----------------------------------------------------
    ports = ["    input  wire i_clock_20M,",
             "    output wire uart_tx,"]
    for vname in RAIL.values():
        ports.append("    output wire %s," % vname)
    ports.append("")
    ports.append("    // Observed pins: INPUT ONLY. Never declared inout, so the")
    ports.append("    // toolchain cannot infer a driver and contend with the MCU.")
    for i, (vname, ball, net, sig) in enumerate(observed):
        comma = "" if i == n - 1 else ","
        ports.append("    input  wire %-14s%s  // %-5s %s" % (vname, comma, ball, net))
    (here / "census_ports.vh").write_text("\n".join(ports) + "\n")

    # ---- observation vector ------------------------------------------------
    obs = ["// Generated by gen_census.py -- do not edit.",
           "localparam integer NPINS  = %d;" % n,
           "localparam integer NBYTES = %d;" % nbytes,
           "",
           "// obs[i] corresponds to index i in pinmap.json.",
           "wire [NPINS-1:0] obs = {"]
    for i, (vname, ball, net, sig) in enumerate(reversed(observed)):
        idx = n - 1 - i
        comma = "" if idx == 0 else ","
        obs.append("    %-14s%s  // [%3d] %-5s %s" % (vname, comma, idx, ball, net))
    obs.append("};")
    (here / "census_obs.vh").write_text("\n".join(obs) + "\n")

    # ---- constraints -------------------------------------------------------
    IN = "{ LOCATION = %s; IOSTANDARD = LVCMOS33; PULLTYPE = NONE; }"
    OUT = "{ LOCATION = %s; IOSTANDARD = LVCMOS33; DRIVESTRENGTH = 20; PULLTYPE = NONE; }"
    a = ["# T76 static census -- generated by gen_census.py, do not edit.",
         "# Observed pins carry PULLTYPE = NONE and no DRIVESTRENGTH: they are",
         "# inputs only and must not load or pull the buses they watch.",
         "",
         "# Sampling clock (independent of the bus under test)",
         "set_pin_assignment\t{ i_clock_20M }\t{ LOCATION = %s; IOSTANDARD = LVCMOS33; PULLTYPE = NONE; PCICLAMP = ON; }" % CLK_BALL,
         "",
         "# UART out to the ISP header",
         "set_pin_assignment\t{ uart_tx }\t%s" % (OUT % UART_BALL),
         "",
         "# Rail control driven to a static safe state (socket MUST be empty)"]
    inv = {v: k for k, v in RAIL.items()}
    for vname in RAIL.values():
        net = inv[vname]
        ball = next(b for b, x in balls.items() if x == net)
        a.append("set_pin_assignment\t{ %s }\t%s" % (vname, OUT % ball))
    a.append("")
    a.append("# Observed pins (net name for each is in pinmap.json)")
    for vname, ball, net, sig in observed:
        a.append("set_pin_assignment\t{ %s }\t%s" % (vname, IN % ball))
    (here / "t76_census.adc").write_text("\n".join(a) + "\n")

    # ---- timing ------------------------------------------------------------
    # Every observed pin is asynchronous to our sampling clock and is passed
    # through a two-flop synchroniser, so cutting these paths is correct rather
    # than merely convenient.
    names = " ".join(v for v, _, _, _ in observed)
    (here / "t76_census.sdc").write_text(
        "create_clock -name clk_20 -period 50.0 [get_ports i_clock_20M]\n"
        "set_false_path -from [get_ports {%s}]\n" % names)

    # ---- testbench wiring --------------------------------------------------
    tb = ["// Generated by gen_census.py -- do not edit.",
          "// Connects every observed port to tb_obs[i], i matching pinmap.json."]
    for i, (vname, ball, net, sig) in enumerate(observed):
        tb.append("    .%-14s(tb_obs[%3d])," % (vname, i))
    (here / "census_tb_connect.vh").write_text("\n".join(tb) + "\n")

    # ---- decode table ------------------------------------------------------
    (here / "pinmap.json").write_text(json.dumps({
        "npins": n, "nbytes": nbytes,
        "clock_ball": CLK_BALL, "uart_ball": UART_BALL,
        "pins": [{"index": i, "name": v, "ball": b, "net": nt, "signal": s}
                 for i, (v, b, nt, s) in enumerate(observed)]}, indent=1) + "\n")

    print("  %d observed pins, %d bytes per bitfield" % (n, nbytes))
    from collections import Counter
    c = Counter("cpu/HSPI" if re.match(r"^(HD|HT|HR|cpu_p)", v) else
                "ZIF" if v.startswith("zif_") else
                "ISP" if v.startswith("j_") else "other" for v, _, _, _ in observed)
    for k, v in sorted(c.items(), key=lambda x: -x[1]):
        print("    %-10s %3d" % (k, v))
    print("  wrote census_ports.vh, census_obs.vh, t76_census.adc, pinmap.json")


if __name__ == "__main__":
    main()
