# Why the T76's SuperSpeed link fails on macOS — the CH569W USB3 implementation

*Researched 2026-08-12, prompted by the bulk-transfer failure documented in
[minipro-setup.md](minipro-setup.md). Live descriptor reads below were taken
from this unit (serial 2023000105) over the working control endpoint.*

**Thesis, now well-supported:** the T76's MCU (WCH CH569W) implements USB 3.0
link power management **in firmware, not silicon**, advertises worst-case
U1/U2 exit latencies copied from WCH's example code, and Apple Silicon's xHCI
is unusually strict about link-layer responsiveness. macOS arms U1/U2, the
firmware misses a wake handshake, and every bulk transfer dies with
`kIOReturnNotResponding`. On USB 2.0 (High Speed) none of that machinery is
used, which is why a USB 2.0 hub is the fix.

## 1. The CH569 services U1/U2 in a firmware interrupt handler

WCH's reference USB3 device stack for the CH56x (mirrored by the HydraUSB3
project, whose BSP is the open copy of WCH's code) handles **every link-state
transition in software** — `LINK_IRQHandler` in
[`CH56x_usb30_devbulk.c`](https://github.com/hydrausb3/wch-ch56x-bsp/blob/main/usb/usb_devbulk/CH56x_usb30_devbulk.c):

- `LINK_GO_U1_FLAG` / `LINK_GO_U2_FLAG` → firmware manually switches the PHY
  power mode (`USB30_switch_powermode(POWER_MODE_1/2)`).
- `LINK_Ux_EXIT_FLAG` (host wants the link back in U0) → firmware must
  reprogram `LINK_CFG` (EQ, de-emphasis, RX terminations) and switch back to
  `POWER_MODE_0` — inside an IRQ on the 120 MHz RISC-V core.
- Hot reset, warm reset, LFPS polling, and even the initial LMP port-capability
  exchange are all serviced the same way.

So the U1/U2 **exit path is software-timed**. If the CPU is busy (the T76
firmware is also driving an FPGA, SPI flash, relays and an ADC) or the
WCH stack mishandles an edge in the LFPS wake handshake, the device simply
does not respond within the exit-latency budget — which at the xHCI level is
reported as *device not responding*, exactly the `kIOReturnNotResponding
(0xe00002ed)` we captured. Parts of WCH's USB3 stack ship only as a binary
blob (`libusb30`), so the community can't audit or fix the remainder.

## 2. The T76 advertises worst-case LPM latencies (live read from this unit)

BOS descriptor read over EP0 on 2026-08-12 (`scratchpad bos_read.py`, raw:
`050f160002071002060000000a1003000e00010aff07`):

| Capability | Value | Meaning |
|---|---|---|
| USB 2.0 Extension `bmAttributes` | `0x06` | claims **LPM + BESL** support on High Speed |
| SuperSpeed cap `bmAttributes` | `0x00` | no LTM |
| `wSpeedsSupported` | `0x000E` | FS/HS/SS (5 Gbps) |
| **`bU1DevExitLat`** | **10 µs (`0x0A`)** | the **maximum legal value** |
| **`wU2DevExitLat`** | **2047 µs (`0x7FF`)** | the **maximum legal value** |

Both exit latencies are the spec ceilings, verbatim from WCH's example
descriptors (HydraUSB3 ships the identical `0x0A / 0x07FF`). Translation: the
firmware tells the host "I support U1/U2, and I may take the worst permissible
time to wake." The host then dutifully enables U1/U2 with those budgets. A
device that then *still* overruns them (see §1) is out of spec, and a strict
host gives up on the transfer.

## 3. Apple Silicon hosts are the strict ones

- macOS decides LPM per device: `ioreg` on this Mac shows
  `kUSBHostDeviceEnableLPM = No` stamped on two hubs (Apple's internal errata
  list), while the **T76 gets no such exemption** — U1/U2 stay armed. The
  errata table lives inside SIP-protected `IOUSBHostFamily`; there is **no
  user-space way** to add a device to it.
- The same failure signature is documented for completely different silicon:
  ST's [STLINK-V3 / STM32 USB fails with `kIOReturnNotResponding` on Apple
  Silicon](https://community.st.com/t5/stm32-mcus-embedded-software/usb-comms-failure-with-apple-silicon-arm-mac/td-p/618571)
  (works on Intel and M1, fails on M3 — Apple's PHY/controller tolerance
  tightened across generations). ST's workarounds: PHY tuning, **forcing a
  lower speed, or a hub** — the same medicine that applies here.
- Device-mode U1/U2 fragility is industry-wide, not a WCH-only sin: Qualcomm
  merged kernel patches [disabling U1/U2 entry on their device controllers](https://lkml.rescloud.iu.edu/2412.2/03868.html)
  because it "can cause packet drops and missed transfers."
- Linux hosts are lenient *and configurable* (`usbcore.quirks=a466:1a86:k`,
  per-device `power/usb3_hardware_lpm` in sysfs); Windows is similarly
  forgiving. That is why Xgpro-on-Windows and minipro-on-Linux work while
  macOS does not. macOS exposes no equivalent knob.

## 4. Corroborating CH569 community experience

- HydraUSB3 (the main open CH569 USB3 effort) sees marginal SuperSpeed link
  training: [hydrausb3_fw #23](https://github.com/hydrausb3/hydrausb3_fw/issues/23)
  — official firmware enumerates USB2-only on some host/cable combinations,
  i.e. the SS link layer is sensitive on this chip even before LPM enters the
  picture.
- radiomanV's [`t76_uploader.py`](https://github.com/radiomanV/Xgecu_T76)
  drives the **same bulk EP 0x01** and lists macOS as supported — evidently
  tested on a host where the link stayed in U0 (Intel Mac, hub, or Linux).
  Consistent with our one unreproducible success: if you catch the link while
  it is awake, bulk works; the first U1/U2 dip kills it.

## 5. Conclusions for this repo

1. **Nothing is wrong with our build or libusb usage.** The failure is a
   firmware (WCH stack) ↔ host-controller (Apple xHCI LPM policy) interaction
   below all user software.
2. **The USB 2.0 hub workaround is principled, not folklore**: at High Speed
   the SS link layer (U1/U2, LFPS, LMP) does not exist. The USB2 path claims
   L1/BESL support too, but that machinery is mature in the CH569's USB2 core.
3. A real macOS-side fix would require Apple adding the T76 (`0xA466:0x1A86`)
   to its LPM errata list, or XGecu either honoring Ux exits in time or
   advertising honest latencies / no LPM support. An XGecu firmware fix is
   plausible — the descriptors and link handling are all in the updatable
   CH569 firmware — but the image is encrypted
   ([firmware-updater.md](firmware-updater.md)), so no community patch is
   possible today.
4. If a future test matters: a **USB 3.0 hub** in the path may *also* mask the
   issue (hubs terminate the upstream link and often re-time LPM), but the
   USB 2.0 hub is the deterministic option.
