# minipro-rs — running many programmers in parallel

*As of 2026-08-17. Companion to [`rust-trait-model.md`](rust-trait-model.md).
The question: can one host drive a bank of T76s for production programming?
The answer is yes, and the trait model already permits it — but one correctness
bug must be fixed first, and it is the kind that produces wrong data rather
than an error.*

## Verdict

| Concern | State |
|---|---|
| Threading model | **Ready.** `Transport: Send`, `Programmer: Send`, no shared mutable state |
| Chip database | **Ready.** `DllDb` is `Send + Sync` and read-only after load — share one `Arc` |
| Device selection | **Missing.** `open_device` takes the first `vid:pid` match |
| Reset re-arm | **Unsafe.** Can silently re-arm onto a *different* programmer |
| Power / bandwidth | **Bounded.** Physical limits bite before the software does |

Nothing here requires re-architecting. The work is one small correctness fix,
one selection API, and a worker pool.

## 1. What already works

The ownership model was built for this, because the TUI already runs operations
off the main thread:

```rust
pub trait Transport: Send { … }     // minipro-core/src/transport.rs
pub trait Programmer: Send { … }    // minipro-core/src/programmer.rs
```

`Send` and not `Sync` is exactly right for a device farm: a programmer moves to
a worker thread and is owned there. Nothing is shared, so nothing needs locking.

There is no global mutable state to serialise on. The only `static` in the
workspace is a `OnceLock<ureq::Agent>` in `minipro-db/src/net.rs`, which is
thread-safe and only touched while fetching the database.

The must-drain `Pending` guard is per-transport, so the hazard it protects
against — *an undrained EP81 response wedges the device until replug* — cannot
propagate between units.

**The database is shareable for free.** `DllDb` is

```rust
pub struct DllDb {
    devices: Vec<Device>,
    index: HashMap<String, usize>,
    algo_dir: Option<PathBuf>,
    algo_path: Option<PathBuf>,
}
```

— plain owned data, no `Rc`, no interior mutability, and read-only once loaded.
That makes it `Send + Sync` automatically, so a single `Arc<DllDb>` serves every
worker. Parsing `InfoICT76.dll` costs ~103 ms and the vendor archive is a 63 MB
download; both should happen **once per host**, not once per programmer.

## 2. The blocker: device selection

`open_device` resolves a device by USB id alone, taking whichever comes first:

```rust
// minipro-usb/src/lib.rs
let info = devices
    .into_iter()
    .find(|d| d.vendor_id() == vid && d.product_id() == pid)
    .ok_or_else(|| Error::Usb(format!("no programmer found ({vid:04x}:{pid:04x})")))?;
```

With four T76s attached, four workers all open the same one. There is no way to
say *which* programmer you mean.

## 3. The correctness bug: reset can re-arm the wrong device

This is the one to fix before anyone builds a rack.

`Transport::reset` re-establishes the connection after the device
re-enumerates — and it does so through the same first-match lookup:

```rust
match open_device(self.vid, self.pid) { … }   // whichever unit answers first
```

On a single-device bench this is correct and invisible. With several
programmers attached it means **a reset mid-job can hand back a different
physical unit than the one you were driving.** The failure mode is not a hang
or an error:

1. Worker A is programming chip A and issues a reset (`reboot`, or recovery).
2. The re-arm lands on programmer B.
3. Worker A writes chip A's image into **chip B**.
4. Worker A verifies by reading back — from B — and the verify *passes*.

Wrong data, silently, with a green result. For a production run that is the
worst available outcome.

The hardware test `live_command_roundtrip_survives_reset` asserts the
system-info device code is unchanged across a reset, which catches this in
CI-with-hardware. **The production path has no equivalent guard.** Adding one is
cheap: remember the identity at open, and refuse to re-arm anything else.

## 4. What identifies a programmer

`nusb`'s `DeviceInfo` exposes, *before* opening (verified against nusb 0.2.7):

| Field | Stable across replug? | Notes |
|---|---|---|
| `serial_number()` | Yes | The right key — **if** the T76 populates the descriptor (unverified) |
| `bus_id()` + `device_address()` | No | Identifies a *port*, not a unit — which may be what a fixed rack wants |
| `product_string()` / `manufacturer_string()` | Yes | Not unique between units |

One caveat worth stating plainly: the serial in `minipro info`
(`4NDKOPR01U1DSHVEH7AQ5935`) comes from the **system-info report**, not the USB
string descriptor. Keying on it means open-then-query — workable (open each
candidate, ask, keep the match) but not a pre-open filter.

**Open question:** does the T76 populate its USB serial string descriptor? This
needs one enumeration against real hardware to settle, and it decides whether
selection can be a cheap pre-open filter or must open-and-interrogate.

## 5. Threads, not processes

In-process threading is the better model, and not only for the shared `Arc<DllDb>`.

Separate processes race on the cache directory. `vendor::ensure_archive` writes
through a temporary file and renames, so the *download* is atomic and safe. But
`extract::unpack_vendor_archive` extracts into a shared destination, and two
processes unpacking concurrently on a cold cache is a genuine race. One process
with N threads avoids the question entirely.

If separate processes are required (isolation, crash containment), pre-warm the
cache with a single run before starting the fleet.

## 6. Physical limits

These will constrain the fleet before the software does.

- **Power.** Each T76 drives socket VCC and VPP. A bus-powered hub will brown
  out with several units under load. Use a powered hub with headroom, and treat
  simultaneous VPP-heavy operations as the worst case. *(Per-unit draw under
  load is unmeasured.)*
- **Bandwidth.** Every session uploads an FPGA bitstream before any chip data
  moves — **~700 KB** for `SPI25F11`, streamed as 512-byte bulk packets.
  High Speed is 480 Mbit/s *shared per host controller*, so reading 16 MB parts
  on several programmers behind one controller serialises on the wire. Put
  banks on separate root hubs.
- **macOS High Speed only.** The T76's SuperSpeed bulk path fails on Apple
  Silicon (see [`ch569-usb3-notes.md`](ch569-usb3-notes.md)); a USB-2 cable
  forces High Speed. That caps per-device throughput and constrains hub
  topology.

A working estimate is **4–8 units per host controller** before bandwidth and
power dominate. This is an estimate, not a measurement.

## 7. Proposed design

```rust
// minipro-usb — identity is captured at open and never re-resolved loosely.
pub struct ProgrammerId {
    pub vid: u16,
    pub pid: u16,
    pub serial: Option<String>,     // USB descriptor, if present
    pub bus: nusb::BusId,           // whatever `DeviceInfo::bus_id` returns
    pub address: u8,
}

pub fn list_programmers() -> Result<Vec<ProgrammerId>>;

impl UsbTransport {
    pub fn open_id(id: &ProgrammerId) -> Result<Self>;
}
```

`UsbTransport` stores the `ProgrammerId` it opened. `reset()` re-opens **that**
device and returns a typed error rather than silently binding to another unit if
it cannot be found again.

CLI surface: `--device <serial>` to pin one unit, and a listing subcommand so an
operator can see what the host sees.

Worker pool: one `Arc<DllDb>`, one thread per `ProgrammerId`, results collected
per device. The existing NDJSON output mode already suits fleet logging — each
line carries its own outcome, so adding a device field makes runs attributable.

## 8. Work items

Ordered by value, not by size.

1. **Pin the identity across `reset`** — S. A correctness fix worth having even
   with one programmer, and independent of everything else here.
2. **`list_programmers()` + `open_id()`** — S/M. The selection API.
3. **Confirm the USB serial descriptor** — S 🔌. One enumeration; decides
   whether selection is pre-open or open-and-query.
4. **`--device` flag and a listing subcommand** — S ⛓️ (needs 2).
5. **Worker pool with a shared `Arc<DllDb>`** — M ⛓️ (needs 2).
6. **Measure the real ceiling** — M 🔌. Per-unit current under VPP load, and
   throughput scaling per host controller. Replaces the estimate in §6.

Legend matches [`rust-roadmap.md`](rust-roadmap.md): **S/M/L** effort ·
🔌 needs hardware · ⛓️ has a dependency.

## Non-goals

- **Coordinating programmers across hosts.** A network-distributed fleet is a
  different problem; nothing here assumes it.
- **Sharing one programmer between threads.** `Programmer` is `Send`, not
  `Sync`, deliberately. One owner per device.
- **Hot-plug during a run.** Detecting units appearing mid-run invites exactly
  the ambiguity §3 is about. Enumerate once, then work the fixed set.
