# T76 USB detection (macOS)

Snapshot from `ioreg -p IOUSB` on 2026-08-12, connected via an Intel USB3 hub:

```
"USB Product Name" = "XGecu T76"
"idVendor"  = 42086   (0xA466)
"idProduct" = 6790    (0x1A86)
"USB Serial Number" = "2023000105"
```

Re-check presence:

```sh
ioreg -p IOUSB -l -w 0 | grep -B2 -A4 'XGecu'
```

Notes:

- No macOS driver binds to it (and none exists) — the device is only usable from the Windows Xgpro application. For VM passthrough, filter on VID `0xA466` / PID `0x1A86`.
- The PID `0x1A86` coincidentally matches the WCH *vendor* ID; the T76 uses a WCH USB interface chip. Don't confuse it with a CH340 serial adapter when setting up passthrough rules.
