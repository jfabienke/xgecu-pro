# minipro-rs — the trait model

*Companion to [rust-redesign.md](rust-redesign.md). This is the load-bearing
design: the traits every crate is built around. Guiding rules — object-safe
(everything selected at runtime is used behind `dyn`), `Send` (the TUI runs ops
on a worker thread), and capability-honest (a driver implements only what the
device can do).*

## Layering

```
Reporter  ◄─────────────── minipro-cli   (human / JSON / TUI renderers)
   ▲                            │ drives
Event/Outcome                   ▼
minipro-core:  Programmer (+ capability sub-traits) · ops orchestration · Error
   │ holds                      │ looks up
   ▼                            ▼
Transport  (minipro-usb)     ChipDb (minipro-db)     Format (file I/O)
```

Four trait families: **`Transport`** (bytes on the wire), **`Programmer`** +
capability traits (what a programmer *does*), **`ChipDb`**/**`Format`** (data),
and **`Reporter`** (how results surface). Concrete types are chosen at runtime,
so each is used as `Box<dyn _>` / `&mut dyn _`.

## 1. Transport — bytes, and the must-drain invariant

```rust
pub trait Transport: Send {
    fn send(&mut self, ep: Ep, data: &[u8]) -> Result<()>;
    fn recv(&mut self, ep: Ep, len: usize) -> Result<Vec<u8>>;
    fn link_speed(&self) -> LinkSpeed;      // Full / High / Super — the macOS tell
    fn reset(&mut self) -> Result<()>;
}
```

Drivers hold a `Box<dyn Transport>`, so the identical driver runs over real USB
(`nusb`) or a `MockTransport` replaying capture fixtures. The C code's load-bearing
hazard — *an undrained EP response wedges the device until replug* — is lifted
into a type you cannot forget to consume:

```rust
#[must_use = "the T76 wedges until USB replug if this response is not drained"]
pub struct Pending<'t> { tx: &'t mut dyn Transport, ep: Ep, len: usize }
impl<'t> Pending<'t> {
    pub fn read(self) -> Result<Vec<u8>> { self.tx.recv(self.ep, self.len) }
    pub fn discard(self) -> Result<()> { self.tx.recv(self.ep, self.len).map(|_| ()) }
}
/// Every command declares its response length up front (as the vendor protocol does).
pub fn command<'t>(tx: &'t mut dyn Transport, out: Ep, r#in: Ep,
                   pkt: &[u8], resp_len: usize) -> Result<Pending<'t>> {
    tx.send(out, pkt)?;
    Ok(Pending { tx, ep: r#in, len: resp_len })
}
```

`#[must_use]` + a consuming `read`/`discard` means "sent a command but skipped
the reply" is a compile-time warning, not a device that hangs.

## 2. Programmer — a thin core + capability sub-traits

The C vtable is ~35 functions, most irrelevant to any given device (JEDEC rows
are PLD-only, partitions are eMMC-only, …). A single fat trait would force every
driver to stub two dozen `Unsupported`s. Instead: a **small always-present core**,
plus **optional capability traits**, discovered at runtime through accessor
upcasts.

```rust
pub trait Programmer: Send {
    fn info(&self) -> &ProgrammerInfo;      // model, firmware, serial, link speed, voltage
    fn caps(&self) -> Caps;                 // bitflags: MEMORY | FUSES | JEDEC | EMMC | LOGIC | PINTEST | FWUPDATE
    fn begin(&mut self, dev: &Device) -> Result<Session>;   // upload bitstream, adapter init, pin config
    fn end(&mut self, session: Session) -> Result<()>;
    fn identify(&mut self, s: &Session) -> Result<ChipId>;
    fn reset(&mut self) -> Result<()>;

    // Capability upcasts. Default `None`; a driver overrides with `Some(self)`
    // for each trait it also implements. Keeps `dyn Programmer` object-safe while
    // exposing heterogeneous optional behaviour.
    fn memory(&mut self)   -> Option<&mut dyn MemoryOps>     { None }
    fn fuses(&mut self)    -> Option<&mut dyn FuseOps>       { None }
    fn jedec(&mut self)    -> Option<&mut dyn JedecOps>      { None }
    fn emmc(&mut self)     -> Option<&mut dyn EmmcOps>       { None }
    fn logic(&mut self)    -> Option<&mut dyn LogicTest>     { None }
    fn pins(&mut self)     -> Option<&mut dyn PinTest>       { None }
    fn firmware(&mut self) -> Option<&mut dyn FirmwareUpdate>{ None }
}
```

A T76 driver: `impl Programmer for T76 { … fn memory(&mut self){ Some(self) } fn emmc(&mut self){ Some(self) } … }`
and `impl MemoryOps for T76 { … }`, `impl EmmcOps for T76 { … }`. The invariant
is simply that `caps()` agrees with which accessors return `Some` (one `debug_assert`
enforces it in tests). Callers do:

```rust
let mem = prog.memory().ok_or(Error::Unsupported("memory ops"))?;
```

### Capability traits (block-granular, like the C `*_block` fns)

```rust
pub trait MemoryOps {
    fn read_block (&mut self, s: &Session, req: &BlockReq) -> Result<Vec<u8>>;
    fn write_block(&mut self, s: &Session, req: &BlockReq, data: &[u8]) -> Result<()>;
    fn erase(&mut self, s: &Session, kind: EraseKind) -> Result<()>;
    fn blank_check(&mut self, s: &Session, region: Region) -> Result<bool>;
}
pub trait FuseOps  { fn read_fuses(&mut self, s: &Session, k: FuseKind) -> Result<Fuses>;
                     fn write_fuses(&mut self, s: &Session, f: &Fuses) -> Result<()>; }
pub trait JedecOps { fn read_row(&mut self, s: &Session, row: u8) -> Result<JedecRow>;
                     fn write_row(&mut self, s: &Session, row: &JedecRow) -> Result<()>; }
pub trait EmmcOps  { fn select_partition(&mut self, s: &Session, p: Partition) -> Result<()>;
                     fn capacity(&self) -> u64; }           // 64-bit; eMMC > u32
pub trait PinTest  { fn contact_check(&mut self, s: &Session) -> Result<PinReport>; } // → BadContact pins
pub trait LogicTest{ fn run(&mut self, s: &Session) -> Result<LogicReport>; }
pub trait FirmwareUpdate { fn update(&mut self, img: &FirmwareImage) -> Result<()>; }
```

Block granularity mirrors the C drivers; the **loop, progress reporting, and
verification live one level up** (§5), not in the driver — so every driver stays
small and the orchestration is written once.

## 3. Session — transaction state, RAII optional

`begin()` returns an owned `Session` token carrying per-transaction state (the
selected algorithm/bitstream handle, eMMC capacity, current partition). Ops
borrow it `&Session`; `end()` consumes it. For ergonomics the core also offers a
`Txn` guard that borrows the programmer and ends on drop:

```rust
pub struct Txn<'p> { prog: &'p mut dyn Programmer, session: Option<Session> }
impl Drop for Txn<'_> { fn drop(&mut self) {
    if let Some(s) = self.session.take() { let _ = self.prog.end(s); } // best-effort
}}
```

So a forgotten `end` can't leave the socket energized — the C code's other
manual-cleanup footgun, closed.

## 4. Data: ChipDb and Format

```rust
pub trait ChipDb {
    fn get(&self, name: &str) -> Option<&Device>;           // exact, e.g. "W25Q64BV@SOIC8"
    fn search(&self, query: &str, limit: usize) -> Search<'_>;  // capped + counted (34k devices)
    fn firmware_target(&self) -> FwVersion;                 // the fw these bitstreams pair with
}
// impls: XmlDb (quick-xml + serde, editable source) · CompiledDb (postcard blob, include_bytes!)

pub trait Format {
    fn parse(&self, bytes: &[u8]) -> Result<Image>;
    fn emit(&self, img: &Image, out: &mut dyn std::io::Write) -> Result<()>;
    fn detect(bytes: &[u8]) -> Option<&'static dyn Format>;  // sniff ihex/srec vs raw
}
// impls: Raw · IHex · SRec · Jedec
```

`Device` is a plain `#[derive(Deserialize)]` struct (the C `device_t`), not a
trait — it's data. `FwVersion` on both `ChipDb` and `ProgrammerInfo` is what
makes the firmware/bitstream mismatch a **typed** check at `begin()`.

## 5. Reporter — one event stream, three renderers

The core never prints. Every op emits `Event`s and returns an `Outcome`; both
`#[derive(Serialize)]`, which *is* the JSON mode.

```rust
pub trait Reporter: Send {
    fn event(&mut self, ev: &Event);       // progress, warnings, notes
    fn finish(&mut self, out: &Outcome);    // terminal result of the op
}
pub enum Event { Progress { done: u64, total: u64 }, Warn(Warning), Note(Cow<'static, str>) }
// Human → indicatif/anstream · Json → NDJSON to stdout · Tui → widget updates
```

Orchestration ties it together — this is where the block loop, progress, and
built-in verification live, generic over any programmer and reporter:

```rust
pub fn read_region(prog: &mut dyn Programmer, s: &Session, region: Region,
                   rep: &mut dyn Reporter) -> Result<Image> {
    let mem = prog.memory().ok_or(Error::Unsupported("memory ops"))?;
    // loop BlockReqs, rep.event(Progress{..}) per block, assemble Image
    // (verification — re-read + crc/sha stability — is a wrapper over this)
}
```

## 6. Errors — typed, with stable JSON codes

```rust
#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("usb transport: {0}")]                    Usb(#[from] UsbError),
    #[error("protocol desync")]                        Protocol,
    #[error("chip id mismatch: expected {expected:04x}, got {got:04x}")]
        ChipIdMismatch { expected: u32, got: u32, alias: Option<String> },
    #[error("bad contact on pins {0:?}")]              BadContact(Vec<u8>),
    #[error("firmware mismatch: device {device}, bitstreams target {target}")]
        FirmwareMismatch { device: FwVersion, target: FwVersion },
    #[error("verify failed at 0x{addr:06x}")]          Verify { addr: u32 },
    #[error("unsupported: {0}")]                       Unsupported(&'static str),
    #[error(transparent)]                              Io(#[from] std::io::Error),
    // …
}
impl Error {
    /// Machine-stable identifier for the JSON `code` field — never localized.
    pub fn code(&self) -> &'static str { /* "chip_id_mismatch", "bad_contact", … */ }
    pub fn hint(&self) -> Option<&'static str> { /* "add --force", "reseat/clean pins" */ }
}
pub type Result<T> = std::result::Result<T, Error>;
```

The JSON reporter renders `{ok:false, code, hint, …fields}` straight off these —
so an agent branches on `code`, never on prose. `hint` is where the hard-won
remediations live (USB-2.0 cable, contact spray, `--force`).

## 7. How it composes — `minipro read` end to end

```rust
let transport = usb::open(vid_pid)?;                     // Box<dyn Transport>
let mut prog  = detect_programmer(transport)?;           // Box<dyn Programmer>
let db        = load_db()?;                              // Box<dyn ChipDb>
let dev       = db.get(&args.chip).ok_or(Error::Unsupported("unknown chip"))?;

if prog.info().firmware != db.firmware_target() {        // typed, not a printf
    rep.event(&Event::Warn(Warning::FirmwareMismatch));  // (or hard-fail per flag)
}
let session = prog.begin(dev)?;                          // bitstream + adapter init
let id = prog.identify(&session)?;                       // pincheck/id, non-fatal per flag
let image = ops::read_region(&mut *prog, &session, Region::code(dev), &mut *rep)?;
let out = Outcome::read(&image, /*reads*/2, /*stable*/true, prog.info().link_speed);
prog.end(session)?;
rep.finish(&out);                                        // human table | NDJSON | TUI panel
```

## 8. Object safety, Send, and testing

- **Object-safe:** no generic methods, no by-value `Self` returns on the `dyn`
  traits; the capability-accessor pattern is what lets optional behaviour stay
  object-safe. `Device`/`Image`/`Outcome` are plain data.
- **`Send`:** `Programmer` and `Transport` are `Send` so the TUI can run a read on
  a worker thread and stream `Event`s to the render thread over a channel (the
  channel's receiver is itself a `Reporter`).
- **Testing:** because drivers only touch `dyn Transport`, `MockTransport`
  replaying the T76 byte-captures turns the protocol into unit tests — the single
  biggest gap in the C codebase — with no hardware in the loop.
