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
minipro-rs/    The Rust redesign and reimplementation (MIT) — see its README
docs/          Open-source status; protocol + firmware notes; design docs
docs/hardware/ FPGA + MCU pinouts and schematic (Anlogic EG4X20 + WCH CH569W)
hardware/      Notes about this unit and its USB identity
```

Proprietary vendor material (the Xgpro installer, XGecu's device-support list,
Anlogic/WCH datasheets) is **not redistributed here** — see the download
sources below and `docs/hardware/README.md`.

## Vendor software (not included)

- `xgpro_T76_V*.rar` → contains `Xgpro_T76_V*.exe` (V13.21 pairs with the
  support list dated 2026-07-11, 42,621 devices). Download from the
  [community mirror](https://github.com/Kreeblah/XGecu_Software).
- The T76 uses its **own installer** (`Xgpro_T76_*`) — it is *not* covered by the regular Xgpro installer for TL866II+/T48/T56.
- Extract with `unar xgpro_T76_V*.rar`.

### macOS note

Xgpro is **Windows-only** (XP–Win11) and drives the programmer directly over USB. On this Mac, run it in a Windows VM (Parallels/VMware/UTM) and pass through the USB device with VID `0xA466` / PID `0x1A86`. An open-source path is emerging — Matt Brown's minipro fork now programs SPI/NOR/NAND/eMMC on the T76 (feature branch). See [docs/open-source-status.md](docs/open-source-status.md), the reverse-engineered [USB protocol](docs/t76-protocol.md), and the [firmware-updater notes](docs/firmware-updater.md).

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

## License

The **original work in this repository is licensed [MIT](LICENSE)** — the
`minipro-rs/` Rust redesign and reimplementation, and the analysis/notes under `docs/`.

Proprietary third-party material — XGecu's Xgpro application and
`InfoICT76.dll`, vendor datasheets, device-support lists, firmware images, and
vendor `.alg` bitstreams — is **not redistributed in this repository** and is
not covered by the MIT grant; the tooling here consumes some of it at runtime
from vendor/mirror sources.

The protocol facts the Rust reimplementation depends on were reverse-engineered by the
minipro community (nmatt0 / Matt Brown); see [`NOTICE`](NOTICE) for full
attribution.
