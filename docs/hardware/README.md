# T76 hardware reference

The T76 is built around an **Anlogic Eagle EG4X20** FPGA (BG256) and a **WCH CH569W** RISC-V MCU (USB 3.0 SuperSpeed). Files sourced from <https://github.com/radiomanV/Xgecu_T76> (The Unlicense).

| File | What it is |
|---|---|
| `DS300_Eagle_Datasheet_en.pdf` | Anlogic Eagle family (EG4) FPGA datasheet |
| `EG4X20BG256_pinout.ods` / `_table.ods` | EG4X20 BG256 package ball-out tables |
| `TD_User_Guide_V4.2_english.pdf` | Anlogic TD IDE user guide (bitstream toolchain) |
| `CH569DS1.PDF` | WCH CH569/CH565 MCU datasheet |
| `CH569W_pinout.ods` / `_table.ods` | CH569W pinout tables |
| `cpu_fpga_schematic.pdf` | CH569W ↔ EG4X20 interconnect schematic |
| `fpga_t76_pinout.ods` / `.pdf` | T76 board-specific FPGA pin mapping |

For custom-bitstream tooling (`gen_bit.py`, `t76_uploader.py`) see the upstream repo and [../open-source-status.md](../open-source-status.md).
