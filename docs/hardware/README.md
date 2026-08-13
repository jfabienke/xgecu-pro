# T76 hardware reference

The T76 is built around an **Anlogic Eagle EG4X20** FPGA (BG256) and a **WCH CH569W** RISC-V MCU (USB 3.0 SuperSpeed). The reverse-engineering artifacts here are sourced from <https://github.com/radiomanV/Xgecu_T76> (The Unlicense).

| File | What it is |
|---|---|
| `EG4X20BG256_pinout.ods` / `_table.ods` | EG4X20 BG256 package ball-out tables |
| `CH569W_pinout.ods` / `_table.ods` | CH569W pinout tables |
| `cpu_fpga_schematic.pdf` | CH569W ↔ EG4X20 interconnect schematic |
| `fpga_t76_pinout.ods` / `.pdf` | T76 board-specific FPGA pin mapping |

**Vendor datasheets are not redistributed here** (Anlogic/WCH copyright). Fetch
them yourself:

| Document | Source |
|---|---|
| Anlogic Eagle (EG4) FPGA datasheet, TD IDE user guide | <https://www.anlogic.com/> (also mirrored in the radiomanV repo above) |
| WCH CH569/CH565 datasheet (`CH569DS1.PDF`) | <https://www.wch-ic.com/products/CH569.html> |

For custom-bitstream tooling (`gen_bit.py`, `t76_uploader.py`) see the upstream repo and [../open-source-status.md](../open-source-status.md).
