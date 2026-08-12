# minipro-rs (scaffold)

A Rust redesign of the [minipro](../minipro-t76) CLI, scaffolded from the design
in [`docs/rust-redesign.md`](../docs/rust-redesign.md),
[`docs/rust-trait-model.md`](../docs/rust-trait-model.md), and the three-mode
output design.

**Status:** stubs. Types, traits, and wiring are in place and compile
warning-free; operation bodies are `todo!()`. It builds **offline, std-only** —
each crate documents the real crates (`nusb`, `clap`, `ratatui`, `serde`, …) it
will pull in when implemented.

```
cargo build          # compiles all 5 crates, zero warnings
cargo test           # MockTransport protocol tests (no hardware)
./target/debug/minipro --json info
```

## Workspace

| Crate | Role |
|---|---|
| `minipro-core` | Device model, `Transport`/`Programmer` + capability traits, `Reporter`, typed `Error`, op orchestration. **std-only** — proves the trait model is object-safe. |
| `minipro-proto` | Per-programmer drivers; `t76.rs` is the reference impl showing the `Some(self)` capability upcasts. |
| `minipro-usb` | `UsbTransport` (over `nusb`) + `MockTransport` for hardware-free tests. |
| `minipro-db` | `ChipDb` trait; `XmlDb`/`CompiledDb` backends (infoic.xml / baked blob). |
| `minipro-cli` | The `minipro` binary: human / JSON / TUI reporters, mode selection. |

## Design in three lines

- **Transport → Programmer(+capabilities) → Reporter**, all `dyn`-dispatched.
- The must-drain `Pending` guard makes "forgot to read the response" (which wedges
  the T76) a compile-time warning.
- One event stream, three renderers; `#[derive(Serialize)]` on the outcome *is*
  the JSON mode.

## What's stubbed vs done

- **Done:** every trait/type signature, the T76 capability wiring, mode dispatch,
  `MockTransport` + two passing protocol tests, the macOS SuperSpeed diagnostic.
- **Stubbed (`todo!()`):** all wire protocol, XML/bitstream parsing, the `nusb`
  and `ratatui` impls, and the block-loop/verification bodies in `ops`.

## Next implementation step

Port the T76 read path first (`begin` → `read_block` → `ops::read_verified`)
against `MockTransport` fixtures captured from the C fork, then swap in the real
`nusb` transport. See [`docs/rust-trait-model.md`](../docs/rust-trait-model.md) §7.
