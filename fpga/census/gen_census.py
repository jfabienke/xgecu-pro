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
UART_BALL = "M4"      # ISP:J08 -- the pin the beacon proved reachable on the
                      # bench (47,936 bytes decoded as "J08"). J19/E13 was the
                      # original choice and was never confirmed on hardware.

# Rail control is DRIVEN to a static safe state, never observed: leaving these
# floating would let the ZIF rail drivers do something undefined.  Polarity is
# not documented, so this is a best guess -- which is exactly why the socket
# must be EMPTY when running the census.  See README.
RAIL = {"T:SCLK": "ser_clk", "T:SDAT": "ser_data",
        "T:LE_VPP": "vpp_le", "T:LE_VCC": "vcc_le", "T:LE_GND": "gnd_le",
        "T:OE_VPP": "vpp_oe", "T:OE_VCC": "vcc_oe", "T:OE_GND": "gnd_oe"}

# Balls the FPGA reserves for configuration/JTAG. TD refuses to place user IO
# on these ("USR-8027 ERROR: Location T2 is for dedicated pin!"), so they cannot
# be observed from user logic at all.
#
# T2 matters beyond the build error: the T76 ball map calls that net "Cpu: 33",
# which mapped to HD17 through the CH569 pinout -- but on the FPGA side it is
# program_b. So that wire is the MCU's FPGA-reconfiguration control line, not a
# bus data line, and the count of wired HSPI data lines drops from 24 to 23.
# HTRDY is the *receiver's* ready line: CH569 Table 10-1 lists it as a
# pull-down INPUT on the MCU ("Detect the status of reception end"), so the FPGA
# must drive it. HTACK is the MCU's own push-pull OUTPUT -- driving that would be
# contention, which is why this distinction is worth the comment.
HTRDY_NET = "Cpu: 18"     # ball T4, CH569 pin 18, PA23/HTRDY

DEDICATED = {
    "T2":  "program_b (FPGA reconfiguration control, driven by the MCU)",
    "C12": "TDI (dedicated JTAG)",
    "A15": "TMS (dedicated JTAG)",
    "C14": "TCK (dedicated JTAG)",
    "E14": "TDO (dedicated JTAG)",
}

# The ISP header's power/ground pins are switched under FPGA control, not hard
# wired. If we drive none of them the header has no ground reference and the
# UART on J19 is unreadable -- the design would look dead when it is running
# fine. radiomanV's working design switches pins 27 and 28 to ground and leaves
# everything else off; we mirror those proven values exactly.
ISP_POWER = {
    "ISP:GND11": ("j_gnd_11", 1),
    "ISP:GND21": ("j_gnd_21", 1),
    "ISP:GND26": ("j_gnd_26", 1),
    "ISP:GND27": ("j_gnd_27", 1),   # ground reference for the UART
    "ISP:GND28": ("j_gnd_28", 1),   # ground reference for the UART
    "ISP:VCC4":  ("j_vcc_04", 0),
    "ISP:VCC20": ("j_vcc_20", 0),
    "ISP:VCC22": ("j_vcc_22", 0),
    "ISP:VCC24": ("j_vcc_24", 0),
    "ISP:VPP24": ("j_vpp_24", 0),
    "ISP:VPP26": ("j_vpp_26", 0),
}

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
        if (ball in (CLK_BALL, UART_BALL) or ball in DEDICATED
                or net in RAIL or net in ISP_POWER or SKIP.match(net)
                or net == HTRDY_NET):
            continue
        # Positions switched to ground are not meaningful observations: they
        # read low because of us. The beacon grounded all five and that is what
        # let the probe's ground lead land anywhere on the header.
        if re.match(r"^ISP:J(11|21|26|27|28)$", net):
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
    for vname, _ in ISP_POWER.values():
        ports.append("    output wire %s," % vname)
    ports.append("    output wire htrdy,   // receiver-ready, driven to the MCU")
    ports.append("")
    ports.append("    // Observed pins: INPUT ONLY. Never declared inout, so the")
    ports.append("    // toolchain cannot infer a driver and contend with the MCU.")
    for i, (vname, ball, net, sig) in enumerate(observed):
        comma = "" if i == n - 1 else ","
        ports.append("    input  wire %-14s%s  // %-5s %s" % (vname, comma, ball, net))
    (here / "census_ports.vh").write_text("\n".join(ports) + "\n")

    # ---- observation vector ------------------------------------------------
    # Sizes live in their own header so the design and the testbench cannot
    # disagree about them after a pinout change.
    # The MCU-link pins sort first (rank 0), so indices 0..NDETAIL-1 are exactly
    # the HSPI/BUS candidate signals. Edge counting is confined to them: they are
    # the only pins whose transition rate we care about, and 32 counters cost a
    # fraction of 103.
    ndetail = sum(1 for v, _, _, _ in observed if re.match(r"^(HD|HT|HR|cpu_p)", v))
    assert all(re.match(r"^(HD|HT|HR|cpu_p)", v) for v, _, _, _ in observed[:ndetail]), \
        "MCU-link pins must occupy the first indices"
    (here / "census_params.vh").write_text(
        "// Generated by gen_census.py -- do not edit.\n"
        "localparam integer NPINS   = %d;\n"
        "localparam integer NBYTES  = %d;\n"
        "localparam integer NDETAIL = %d;   // MCU-link pins, indices 0..NDETAIL-1\n"
        % (n, nbytes, ndetail))

    obs = ["// Generated by gen_census.py -- do not edit.",
           "// obs[i] corresponds to index i in pinmap.json.",
           "wire [NPINS-1:0] obs = {"]
    for i, (vname, ball, net, sig) in enumerate(reversed(observed)):
        idx = n - 1 - i
        comma = "" if idx == 0 else ","
        obs.append("    %-14s%s  // [%3d] %-5s %s" % (vname, comma, idx, ball, net))
    obs.append("};")
    (here / "census_obs.vh").write_text("\n".join(obs) + "\n")

    # ---- HD capture bus ----------------------------------------------------
    # hd_bus[k] is HDk for k in 0..22 (HD17 is program_b and unavailable, tied 0),
    # and hd_bus[23] is HD31 -- the only wired line above HD22.
    sig_of = {}
    for vname, ball, net, sig in observed:
        sig_of[sig] = vname
    bits = []
    for k in range(23):
        bits.append(sig_of.get("HD%d" % k, "1'b0"))
    bits.append(sig_of.get("HD31", "1'b0"))
    hb = ["// Generated by gen_census.py -- do not edit.",
          "// hd_bus[0..22] = HD0..HD22 (HD17 unavailable, tied 0); hd_bus[23] = HD31.",
          "wire [23:0] hd_bus = {"]
    for k in range(23, -1, -1):
        lbl = "HD31" if k == 23 else "HD%d" % k
        hb.append("    %-12s%s  // [%2d] %s" % (bits[k], "" if k == 0 else ",", k, lbl))
    hb.append("};")
    (here / "census_hd_bus.vh").write_text("\n".join(hb) + "\n")

    # ---- ISP header power/ground drive ------------------------------------
    isp = ["// Generated by gen_census.py -- do not edit.",
           "// ISP header pins 27 and 28 are switched to ground so the header has a",
           "// reference for the UART on J19; everything else stays off."]
    for vname, val in ISP_POWER.values():
        isp.append("assign %-10s = 1'b%d;" % (vname, val))
    (here / "census_isp_power.vh").write_text("\n".join(isp) + "\n")

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
    a.append("# ISP header power/ground switches (27, 28 -> ground)")
    for net, (vname, _) in ISP_POWER.items():
        ball = next((b for b, x in balls.items() if x == net), None)
        if ball:
            a.append("set_pin_assignment\t{ %s }\t%s" % (vname, OUT % ball))
    a.append("")
    a.append("# HTRDY: the one MCU-facing pin this design drives (see CH569 Table 10-1)")
    htrdy_ball = next((b for b, x in balls.items() if x == HTRDY_NET), None)
    if htrdy_ball:
        a.append("set_pin_assignment\t{ htrdy }\t%s" % (OUT % htrdy_ball))
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

    # ---- testbench signal indices ------------------------------------------
    # So a testbench can drive named HSPI signals through tb_obs without
    # hard-coding indices that shift whenever the pinout changes.
    idx = {v: i for i, (v, _, _, _) in enumerate(observed)}
    ti = ["// Generated by gen_census.py -- do not edit."]
    for k in ["HTCLK", "HTREQ", "HTVLD"] + ["HD%d" % n for n in range(23)] + ["HD31"]:
        if k in idx:
            ti.append("localparam integer IDX_%-6s = %3d;" % (k, idx[k]))
    (here / "census_tb_idx.vh").write_text("\n".join(ti) + "\n")

    # Explicit per-line drive statements. HD numbers are NOT contiguous in the
    # observed ordering (names sort alphabetically: HD0, HD1, HD10, ...), so a
    # testbench that assumes IDX_HD0 + k silently scrambles the payload.
    dv = ["// Generated by gen_census.py -- do not edit.",
          "// Body of drive_hd(w): put a 24-bit word on the HD lines."]
    for k in range(23):
        nm = "HD%d" % k
        if nm in idx:
            dv.append("tb_obs[%3d] = w[%2d];   // %s" % (idx[nm], k, nm))
    if "HD31" in idx:
        dv.append("tb_obs[%3d] = w[23];   // HD31" % idx["HD31"])
    (here / "census_tb_drive.vh").write_text("\n".join(dv) + "\n")

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
        "dedicated_excluded": DEDICATED,
        "ndetail": ndetail,
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
