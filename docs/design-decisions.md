# minipro-rs — design decisions, and how they fared

*A retrospective. [`rust-redesign.md`](rust-redesign.md) is the plan this project
was built from and [`rust-trait-model.md`](rust-trait-model.md) the architecture
as built; this document records the **decisions** — what was chosen, why, what
nmatt0's C fork does instead, and what happened when each decision met real
silicon. The last column is the honest one: several decisions looked right for
months and were proven wrong in an afternoon of hardware testing.*

*C throughout = [nmatt0's `t76-improvements` minipro fork](https://gitlab.com/nmatt0/minipro/-/tree/t76-improvements),
the reference implementation this project is checked against.*

## The one meta-decision

Everything below descends from one choice: **treat the C implementation as the
protocol oracle, and diverge from it only deliberately.** The C tool has years
of field use; its wire behaviour is evidence. Our divergences fall into three
kinds, and the record is unambiguous about which kind goes well:

| Kind | Examples | Outcome |
|---|---|---|
| **Deliberate, measured** | per-command deadlines; refusing OTP erase before opening the device | Worked, and improved on the C |
| **Deliberate, unmeasured** | config-cycling every open (a SuperSpeed workaround applied at all speeds) | Wedged the device on alternating runs |
| **Accidental** | per-block init with `block_count = 1`; pin check run mid-session | Writes programmed nothing; every read corrupted |

The lesson written into the code and this document: *a divergence from the C
needs either a capture or a measurement to justify it.* Matching the C is the
default; beating it requires evidence.

Stated as a project rule: **wire facts come only from the C implementation,
from USB captures, or from measurements against our own hardware — never from
guesswork.** Where a fact is missing, the tool refuses or stops rather than
inventing traffic (the self-test stimulus and ISP-over-VGA are unimplemented
for exactly this reason). Host-side *policy* may diverge when measured —
timeouts, refusals, verification — but the bytes on the bus are always
someone's observed fact. This discipline is also what the MIT license rests
on: the drivers are independent expression of protocol facts, not of the C's
code.

## 1. Pure-Rust USB (`nusb`) instead of libusb

**Decision.** No C dependencies: `nusb` for USB, `flate2`/`miniz_oxide` for the
bitstream inflate. **C:** libusb-1.0 + zlib via pkg-config.

**Why.** The C tool's worst user experience is not protocol bugs — it is
`pkg-config` on macOS and libusb version skew. A `cargo build` that needs
nothing else, and a static musl binary on Linux, removes the whole class.

**How it fared.** The decision held, but its *workaround* became the week's
nastiest bug. nusb on macOS needed a configuration cycle (`set_configuration(0)`
then `(1)`) to re-arm the T76's bulk endpoints at SuperSpeed — a problem libusb
users never see because the C tool is typically run with a USB-2 cable. We
applied that cycle on **every** open. At High Speed it is not merely
unnecessary: after a completed session it wedges the device, so every other
invocation froze. Four consecutive clean runs of the C tool — which does no
config cycle — is what localized it. Now gated on `LinkSpeed::Super`.

Verdict: right decision, and a reminder that a workaround is itself a
divergence needing a guard.

## 2. The must-drain `Pending` guard

**Decision.** A sent command returns a `#[must_use]` guard that can only be
resolved by draining the reply; forgetting is a compile-time warning, promoted
to an error workspace-wide. **C:** the same invariant lives in a comment
("drain the response"), enforced by review.

**Why.** An undrained EP 0x81 reply wedges the T76 until replug. That is a
hardware fact; encoding it in the type system means the 200th contributor
cannot recreate the bug the first one fixed.

**How it fared.** No undrained-reply wedge has occurred in any hardware
session. The wedges we *did* hit came from elsewhere (the config cycle, §1) —
which is itself the point: the class of bug the type prevents stayed prevented.

## 3. Golden-packet tests as the spine — and their measured limit

**Decision.** Every driver is pinned by byte-exact tests against captured wire
traffic (203 at time of writing), runnable with no hardware. **C:** effectively
no tests; correctness rests on field use.

**Why.** The protocol was reverse-engineered from captures; freezing the wire
output *as data* means refactors cannot silently change what goes on the bus.
It is also what made an MIT clean-room reimplementation reviewable.

**How it fared.** Both halves proved out, and the limit is now measured rather
than suspected. The suite caught every refactor regression this week at zero
hardware cost. It also **passed while the write path programmed nothing**: the
packets were byte-plausible, the cadence was wrong (§4), and no assertion could
tell "wrote correctly" from "wrote nothing". Golden packets are necessary and
not sufficient; silicon is the only test that counts. The README now says this
in as many words.

## 4. Block operations: where the C's shape was load-bearing

**Decision (as originally made).** A minimal `BlockReq { kind, address, len }`
per block — simpler than the C's `data_set_t`, which also carries `init` and
`block_count`. The C sends one init announcing the whole transfer, then streams
blocks; we sent an init *per block*, each announcing a count of 1.

**Why (at the time).** It looked like protocol bookkeeping the caller shouldn't
carry, and reads — the only hardware-verified path then — worked fine without it.

**How it fared.** This was the project's most instructive failure. Reads
tolerate the wrong cadence because the chip is passive: a re-opened one-block
read stream still clocks data out. Programming is stateful, and the device
never entered a program cycle: a full 256 KiB write completed "successfully"
and changed **zero bits** on an MX27C2000. The C's extra two fields were not
bookkeeping — they were the protocol. `BlockReq` now carries `init` and
`block_count`, the ops loops populate them, and the same write programs and
verifies bit-exactly.

Verdict: simplifying away something the reference carries is an *accidental*
divergence even when done knowingly — the C's shape encoded a wire fact nobody
had written down.

## 5. Timeouts: inherited, then measured, then diverged

**Decision (final form).** Deadlines are per-command, not per-endpoint: every
ordinary reply gets 5 s; only the commands that genuinely block on the chip —
full-chip and fuse erase — get the C's 360 s, via an explicit `command_slow`.
**C:** a blanket 360 s on every command reply (`MP_USB_READ_TIMEOUT`).

**Why.** We first inherited the C's value verbatim. Then hardware debugging made
its cost concrete: a wedged device did not *fail*, it *froze* — 90 s per probe
during diagnosis, six minutes in production. Measured on a live T76 across four
operation types, no ordinary reply took longer than 200.4 ms (p50 47 ms), so
the blanket deadline was ~1800× the observed worst case.

**How it fared.** This is the model deliberate divergence: measure first, keep
the C's long deadline exactly where its justification holds, make the fast path
fail fast everywhere else, and pin the routing with tests so it cannot silently
invert. A stalled device now yields a typed error in seconds.

## 6. Honest refusal over best-effort attempt

**Decision.** When the tool cannot do something meaningful, it says so before
touching the hardware, with a stable machine code and a reason: `@ISP_VGA`
parts (programmed down a display cable we don't drive), erasing OTP parts (the
database's `CAN_ERASE` bit, which we carried but — like an embarrassing number
of things — never consulted), an idle SPI bus reported as `no_device_response`
rather than "unknown vendor". **C:** refuses VGA parts and gates erase on
`flags.can_erase`; reports `0x00` JEDEC ids as-is.

**Why.** A failure downstream of a doomed attempt produces an error about the
wrong thing. Worse, some doomed attempts *succeed*: autodetect happily reported
a floating bus as a detected chip.

**How it fared.** Three of this week's fixes were exactly this class, and in two
of them the C already refused where we did not — the oracle was ahead of us on
its own database's semantics.

A full audit against the C's decoder followed. It decodes **ten** semantics from
`raw_flags`; we consulted two. The complete set is now named in
`device::flags`, and the audit found one live landmine in the unconsumed
remainder: `OFF_PROTECT_BEFORE`, carried by **10,123 devices** (30% of the
catalog, **mainstream SPI NOR included** — a W25Q128BV carries it), whose
absence from the write path means — in the C's own words — "the program
silently does nothing" on protected parallel NOR. The first response was a
refusal; a test against real catalog flag words immediately showed that would
refuse most flash writes, so the actual sequence went in instead:
`ops::lift_protect` mirrors the C's protect-off / end / begin cycle, pinned by
tests, honestly marked *unverified on silicon* until a protect-carrying part is
on the bench. The other still-unconsumed semantics (16-bit `DATA_BUS_WIDTH`,
9,256 devices; the data-region offset; write-only lock bits; calibration) are
documented on their accessors as open rather than left invisible. A follow-up
audit then *shrank* one of them: tracing every `word_size` consumer in the C
showed `code_memory_size` is bytes for all parts and the flag never reaches the
drivers, so 16-bit memory transfers were already correct here — the residue is
fuse item width and display, relevant only when fuse operations surface.

While consolidating, both gate families moved to where a design can enforce
them: every capability refusal now lives in `ops::preflight` (one testable
function, shared by any frontend — they had been CLI-local, which a growing
TUI would have silently missed), and the block plan that broke writes lives in
`Region::blocks` (one iterator; five call sites had been re-deriving
`init`/`block_count` by hand).

## 7. Typed errors, one event stream, three renderers

**Decision.** The core never prints. Operations emit events and outcomes;
human, NDJSON, and TUI renderers consume the same stream. Errors are typed,
with a stable `code` contract (never localized, safe to branch on) and a
`hint` naming the next step. **C:** printf to stderr, `int` returns, one
console format.

**Why.** Scripts and agents should not scrape a console; and remediation
knowledge ("on macOS use a USB-2 cable") belongs in the error, not in a wiki.

**How it fared.** The JSON mode quietly became the project's own debugging
instrument — the alternating-hang bug was characterized by scripting the CLI
and diffing NDJSON outcomes, which console scraping would have made miserable.
The `code` contract also forced honesty: `no_device_response` exists because
`ok:true` with a garbage id was a lie the schema made visible.

## 8. The database: vendor files as the only format

**Decision.** Parse XGecu's `InfoICT76.dll` directly; cache the vendor files
themselves; no derived format. Fetch the vendor's installer from the community
mirror at runtime; unpack with a RAR tool the system already has. **C:** a
separate generation step produces a 48 MB `algorithm.xml` the tool then loads;
libusb-style external deps for decompression.

**Why.** Every derived format is a schema to version and a regeneration step to
forget. Measured: parsing the DLL costs 103 ms against 97 ms for the derived
blob it replaced — 6 ms was not worth a format. Not bundling a RAR decoder is
also what keeps the binary MIT-clean, and not redistributing the database is
what keeps the project honest about whose data it is.

**How it fared.** The zero-setup path has worked since it landed. The one
genuine bug was ours, and instructive: `Nand_`/`Vga_` spelled `NAND_`/`VGA_`
resolved fine on macOS and would have failed on every Linux machine — 3,275
devices, invisible on the development platform. A `catalog_coverage` test now
walks all 33,612 devices against a real archive so that class cannot return.

## 9. No panics, enforced; no unsafe, forbidden

**Decision.** `unwrap`/`expect`/`panic` denied by workspace lints (tests
exempt); `#![forbid(unsafe_code)]`. Device replies are parsed through total
accessors; malformed input is a typed error. **C:** fixed buffers, manual
frees, and `exit()` on some paths.

**How it fared.** Property tests fuzz every op against arbitrary replies with
no panics, and the firmware *does* send garbage — its reply buffer is never
zeroed, so stale bytes from previous replies (an ASCII date, once) arrive
where payloads are expected. The discipline that looked like ceremony is what
made "decode stale buffer as pin data" a diagnosis instead of a crash.

## 10. What the C still does better

Kept explicit, because this document would be dishonest without it:

- **Breadth**: five programmer families vs our three; TL866II+/A/CS untouched.
- **Field-proven depth**: years of users across thousands of chip types. Our
  hardware verification is two EPROM classes on one T76 — real, but narrow.
- **The bitbang/custom-protocol path** (`flags.custom_protocol`) we have not
  implemented at all.
- **It was right where we were wrong**, twice, on its own database semantics
  (`can_erase`, VGA refusal) — and its steady 4/4 clean runs were the control
  that localized our worst transport bug.

The relationship is not rivalry. The C fork is the reference that makes
verifying a reimplementation possible at all; the reimplementation, in turn,
has surfaced findings that apply upstream (the blanket timeout, the `Nand_`
case sensitivity on case-sensitive filesystems, the T76 pin check reading
stale buffer). See [`NOTICE`](../NOTICE) for the attribution that underpins
all of this.

## The compressed version

1. **Match the reference by default; diverge only with a capture or a
   measurement in hand.** Accidental divergence cost us writes and reads;
   measured divergence gave us better timeouts than the original.
2. **Encode hardware facts in types** (must-drain, capability gates) — the
   encoded ones stayed fixed, the commented ones did not exist to save us.
3. **Golden packets pin the wire; only silicon proves the operation.** Both
   are load-bearing; neither substitutes for the other.
4. **Refuse honestly, early, with a reason** — a correct error beats a
   plausible success.
5. **Prefer the vendor's own data over derived formats**, and measure before
   optimizing one into existence.
