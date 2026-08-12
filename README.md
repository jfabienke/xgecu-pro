# XGecu Pro T76

Support repo for the **XGecu T76** universal programmer (NAND / eMMC / NOR / EPROM / MCU / ISP, USB 3.0).

## This unit

Detected on USB on this Mac (2026-08-12):

| Property | Value |
|---|---|
| Product name | `XGecu T76` |
| USB Vendor ID | `0xA466` (42086) |
| USB Product ID | `0x1A86` (6790) |
| Serial number | `2023000105` |

Re-check with: `ioreg -p IOUSB -l -w 0 | grep -A4 'XGecu'` — see [hardware/usb-detection.md](hardware/usb-detection.md).

## Repo layout

```
software/      Official Xgpro T76 application (Windows installer, RAR-packed)
docs/          User guide / manuals; open-source status
docs/hardware/ FPGA + MCU datasheets, pinouts, schematic (Anlogic EG4X20 + WCH CH569W)
chip-support/  Official device support list (searchable text)
hardware/      Notes about this unit and its USB identity
```

## Software

- `software/xgpro_T76_V1321.rar` → contains `Xgpro_T76_V1321.exe` (**V13.21**, matching the support list dated 2026-07-11, 42,621 devices).
- The T76 uses its **own installer** (`Xgpro_T76_*`) — it is *not* covered by the regular Xgpro installer for TL866II+/T48/T56.
- Extract with `unar software/xgpro_T76_V1321.rar`.

### macOS note

Xgpro is **Windows-only** (XP–Win11) and drives the programmer directly over USB. On this Mac, run it in a Windows VM (Parallels/VMware/UTM) and pass through the USB device with VID `0xA466` / PID `0x1A86`. No open-source alternative supports the T76 yet — see [docs/open-source-status.md](docs/open-source-status.md) for the current state and which projects to watch.

## Sources

- Official site: <http://www.xgecu.com/en/download.html> (frequently unreachable outside China — was down at time of writing)
- Official forum download thread: <http://forums.xgecu.com/viewthread.php?tid=20>
- Community mirror (software + support lists, kept current): <https://github.com/Kreeblah/XGecu_Software>
- User guide source (T48/T76 Xgpro guide): <https://probots.co.in/technical_data/XGecu%20T48%20Universal%20Programmer__Guide.pdf>

### Updating

Newer `xgpro_T76_V*.rar` builds appear in the mirror under `Xgpro/<major>/`:

```sh
curl -s "https://api.github.com/repos/Kreeblah/XGecu_Software/git/trees/master?recursive=1" \
  | grep -o '"path": *"[^"]*T76[^"]*"'
```
