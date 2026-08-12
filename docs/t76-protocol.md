
<!--
Source: Matt Brown (nmatt0), minipro fork, branch `t76-improvements`, file t76.md
  https://gitlab.com/nmatt0/minipro/-/blob/t76-improvements/t76.md
Vendored here verbatim for reference. See ../docs/open-source-status.md for context.
-->

# T76 Protocol Documentation #


The XGecu T76 is an FPGA-based programmer. Unlike the TL866 series it
exposes a single bulk-only WinUSB interface (VID `0xa466`, PID `0x1a86`)
with two endpoint pairs: the EP1/EP81 pair carries commands and small
responses, and the EP5/EP82 pair carries bulk payloads (chip data and
FPGA bitstreams) after a setup command on EP1. A third IN endpoint,
EP83, exists on the interface but is not used by minipro.

| EP   | Dir | Purpose                                  |
|:----:|:---:|------------------------------------------|
| 0x01 | OUT | Command channel (8-128 byte messages)    |
| 0x81 | IN  | Command response                         |
| 0x05 | OUT | Bulk payload write (programming)         |
| 0x82 | IN  | Bulk payload read (dumping)              |

The protocol is shared across four XGecu programmers (TL866II+, T56, T48,
T76), selected by a model byte; the T76 is model `8`.

## Command Bytes ##

* 0x00 - Get system (programmer) info
* 0x02 - Logic/NAND begin prelude (sent before 0x03 for some chip types)
* 0x03 - Begin transaction (**128 bytes**, see below)
* 0x04 - End transaction
* 0x05 - Read chip id
* 0x06 - Read USER
* 0x09 - Write CONFIG
* 0x0C - Write payload to code memory (bulk array program)
* 0x0D - Read payload from code memory (bulk array read)
* 0x0E - Erase chip / block / NAND die
* 0x10 - Read data memory (small addressed reads: config/SFDP/status)
* 0x11 - Write data memory (small addressed program)
* 0x18 - Protect off
* 0x1D - Read JEDEC fuse row
* 0x1E - Write JEDEC fuse row
* 0x1F - NAND per-block program
* 0x24 - FPGA register I/O (adapter detection)
* 0x26 - Write FPGA bitstream
* 0x27 - eMMC command tunnel
* 0x28 - Logic IC test vector
* 0x39 - Request status (over-current + verify error)
* 0x3A - NAND read bad-block marker
* 0x3E - Hardware pin contact test
* 0x3F - Reset device to normal mode

## Get Programmer Information ##

Sending command 0x00 (8 bytes) returns a 64-byte structure describing the
device, including firmware version, model and the supply voltage. The
layout is:

```
struct t76_report_info
{
	uint8_t  unused1;
	uint8_t  revision;
	uint8_t  unused2[2];
	uint8_t  firmware_version_minor;  // 0x04
	uint8_t  firmware_version_major;  // 0x05
	uint8_t  device_type;             // 0x06, 8 = T76
	uint8_t  unused3;
	char     manufacture_date[16];    // 0x08, ASCII
	uint8_t  device_code[8];          // 0x18
	uint8_t  serial_number[24];       // 0x20
	uint32_t voltage_mv;              // 0x38, millivolts (5000 = 5.000 V)
	uint8_t  usb_speed;               // 0x3C, hard-coded 3, cosmetic only
	uint8_t  unused4;
	uint8_t  external_power;          // 0x3E
	uint8_t  unused5;
};
```

For the T76 the device type is 8. minipro expects firmware 00.1.17
(0x111). The `usb_speed` byte always reads 3 on real hardware regardless
of the actual link speed, so infer real link speed from the USB
descriptor instead.

## Transactions ##

A transaction is opened before any read, write or erase. The command is
0x03, but unlike the TL866II+ (which sends a 64-byte structure) the T76
needs a full **128-byte** packet. The first 64 bytes carry the chip
parameters (chip type, voltages, geometry, flags, package details), and
the second 64 bytes (`msg[0x40..0x7F]`) carry a **chip-class-specific
extension** that programs the FPGA's read/write setup. This extension is
the critical difference: without it the FPGA has no valid bit-bang setup,
so SPI reads clock out all zeros, READID returns 0x0000 and erase is a
no-op. The layout of this extension depends on the chip class.

```
struct t76_begin_transaction {           // 128 bytes
	uint8_t  cmd;                 // 0x00, 0x03
	uint8_t  chip_type;           // 0x01, e.g. 0x03 SPI25, 0x12 NOR/NAND, 0x2d Logic, 0x31 eMMC
	uint8_t  model_selector;      // 0x02
	uint8_t  icsp;                // 0x03
	uint16_t voltages;            // 0x04, raw VPP/VCC code
	uint16_t chip_info;           // 0x06
	uint16_t data_memory2_size;   // 0x08
	uint16_t page_size;           // 0x0A
	uint16_t pulse_delay;         // 0x0C
	uint16_t extra;               // 0x0E
	uint32_t code_memory_size;    // 0x10
	uint8_t  reserved1[0x14];     // 0x14
	uint32_t package_details;     // 0x28
	uint32_t read_buffer_size;    // 0x2C
	uint32_t raw_flags;           // 0x30
	uint32_t chip_family;         // 0x34
	uint32_t variant;             // 0x38, (algo_number << 8) | model
	uint32_t adapter_flags;       // 0x3C
	uint8_t  extension[0x40];     // 0x40, chip-class-specific (FPGA setup)
};
```

Close the session with end transaction (0x04), an 8-byte command.

The chip-class extensions minipro currently implements:

| Chip class             | chip_type / protocol | Status |
|------------------------|----------------------|--------|
| SPI 25-series NOR      | 0x03 (3/0xF)         | read / erase / program |
| Parallel NOR (x16)     | 0x12 / 0x14          | read / erase / program |
| NAND (parallel + SPI)  | 0x2d                 | read / erase / program (no host ECC) |
| eMMC                   | 0x31                 | read / erase / program |

## Reading / Writing ##

Bulk transfers use a split that mirrors read and write:

* **0x0D READ_CODE** and **0x0C WRITE_CODE** are the bulk array paths. A
  single 16-byte init command carries the block size and block count, then
  the whole image streams on EP82 (read) or EP05 (write). For a read the
  init is `[0x0D | block_size(2) @0x02 | start_addr(4) @0x04 | block_count(4) @0x08]`;
  the device then streams `block_count × block_size` bytes on EP82.
* **0x10 READ_DATA** and **0x11 WRITE_DATA** are small, addressed accesses
  (config / SFDP / status registers), not the bulk path. The read returns
  `block_size + 16` bytes on EP82, where the first 16 bytes are a header.

For SPI NOR the actual read setup lives in the BEGIN_TRANS extension, not
in the 0x0D command, so once the 128-byte BEGIN_TRANS is correct the
bulk read path just works.

NAND and eMMC layer on top of this. NAND programs per block via 0x1F
(per-page `[16-byte header | page+spare]` on EP05, then a 0x39 commit per
block) and erases per block via 0x0E, skipping bad blocks flagged by 0x3A.
eMMC tunnels its control commands (partition switch, status, EXT_CSD)
through 0x27 while bulk region data uses the 0x0D / 0x1F path in 64 KiB
blocks. See `src/t76.c` for the implementation details.

## FPGA Bitstreams ##

The T76 is FPGA-based, so most chip families require a per-chip bitstream
uploaded over opcode 0x26 (BEGIN / BLOCK / END / RESET) before the chip
session. minipro sources these from `algorithm.xml`, which is built from
the XGPro_T76 install (see `dump-alg-minipro.bash` / `build-algorithm-xml.sh`).
The bitstreams are firmware-version specific: a mismatched bitstream
mis-configures the FPGA and presents as a bad or empty chip-ID read, so
`algorithm.xml` must match the device firmware (V13.19 pairs with 00.1.17).
