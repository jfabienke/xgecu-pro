# FPGA toolchain state

What it takes to actually build a bitstream for the T76's **EG4X20BG256**, and
where that stands. Written 2026-08-23.

## There is no open-source path

Checked directly, not assumed:

- **nextpnr has no Anlogic architecture.** A local checkout carries `ice40`,
  `ecp5`, `nexus`, `machxo2`, `mistral`, `generic`, and `himbaechel` (uarches
  `example`, `gatemate`, `gowin`, `ng-ultra`, `xilinx`). No Eagle, and no source
  file mentions Anlogic.
- **prjtang is bitstream tooling, not a flow.** It ships `tangbit`, `tangpack`,
  `tangunpack` and a documented container format. Its README claims a
  Yosys + nextpnr flow, but "Getting Started" and "Current Status" are empty
  headings, there is no nextpnr integration in the tree, and the last commit is
  2022-12-25.
- **The bootstrap is circular.** `create_database.py` requires `TD_HOME=/opt/TD`
  — generating the open database needs the proprietary tool it would replace.

prjtang is still useful for reading bitstreams: its `devices.json` names our
exact part — `EG4X20BG256`, family `eagle_20`, package `BGA256X`, idcode
`0x00014c35`, 1259 frames, 3904 bits/frame, 9216 bram bits/frame — matching the
header of radiomanV's prebuilt `T76.bit` (`Architecture: eagle_20`,
`Package: BGA256X`, TD `Version: 5.0.28716`).

## So: Anlogic TD, which needs an account

TD runs on **Windows 7 SP1+ or Red Hat 6.0+ x86_64** — no macOS build. It
supports TCL batch flow (`Project → Export Tcl File`, then `source demo.tcl`),
so headless container builds are fine.

**Downloads require a free Anlogic account.** Clicking a TD file while logged
out returns *"Sorry, you have not logged in!"*. The registration-free exemption
the site advertises covers **FamilyOverview documents** for the EF2/EF3/EG4
series — datasheets, not the software. There is also a separate **TD License**
category containing `license.lic`, which is what addresses the known
`RUN-003 ERROR: License expired!` failure.

Creating the account has to be done by a person; it is not something the
assistant will do.

### Files needed

From **Software Tools → TD Linux → TD_4.6 / TD_5.6**, and **TD License**:

| File | Version | SHA-256 (as published) |
|---|---|---|
| `TD_5.6.5_Release_119222_NL.zip` | 5.6.5 | `a2f73e15ab3ceec0a8a174111a55319061bee7f9288b06eb015cd59509a03524` |
| `TD_4.6.8_SP1_116866_NL.zip` | 4.6.8 | `12f8c2b27393fe43ad9d38563dff3d68e0ee0d3349fb35a9ac9a438a4f933f15` |
| `TD_4.6.8_CPLD_Release_96021_NL.zip` | 4.6.8 | `f99d3c9cc05cdd6e3032aca01551b6a8ff2958e52d3e1fbbd99b7942e29b65f3` |
| `TD_6.0.2_PHX_Release_117864_NL.zip` | 6.0.2 | `62ec83528ae57ffcddc83b2fd30d1a0d93c1fb18e736f6f597238d32de0e207f` |
| `license.lic` | dated 2025-12-10 | — |

**Prefer 5.6.5.** radiomanV's working T76 bitstream was built with TD 5.0.28716,
so the 5.x line is proven against this exact part; 5.6.5 is the nearest
available. Keep 4.6.8_SP1 as a fallback — Anlogic associates the 4.6 line with
EG4 — and skip 6.0.2, which is the Phoenix branch.

Verify with `shasum -a 256` against the table before unpacking.

## Container

An OrbStack machine is already built and validated for this:

```sh
orb -m td-x86 bash        # Ubuntu 26.04 LTS, x86_64
```

- `uname -m` = `x86_64`, 52 GB free
- Rosetta emulation measured at **~1.4× native** (0.193 s vs 0.141 s on the same
  benchmark), so TD's runtime is not a concern
- Every legacy library a RHEL6-era EDA binary tends to want — `libtinfo5`,
  `libpng12-0`, `libncurses5`, the X11/GL set — is available from Ubuntu's
  archive
- `megatools` and the Python crypto stack are installed

Drop the TD zip anywhere under the Mac filesystem and unpack it inside the
machine. **TD and anything derived from it stays in the container** — never in
this git tree, consistent with how the Xgpro installer and the vendor DLL are
handled.

## Mirror status

The Sipeed mega.nz share that prjtang points at (`5AAiSBwB`) **no longer hosts
TD**. Enumerated via MEGA's API: 446 nodes, of which 431 are folders and 15 are
files; the folder prjtang links to (`ZY50yRhI`) contains one empty `IDE`
directory, and the whole `TANG` tree has no files at any depth. The only files
left in the share are unrelated Sipeed board images and datasheets. The
structure survived; the payload is gone.
