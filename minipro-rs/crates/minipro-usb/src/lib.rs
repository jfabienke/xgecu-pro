// SPDX-FileCopyrightText: 2026 John Fabienke
// SPDX-License-Identifier: MIT

//! Transport implementations.
//!
//! [`UsbTransport`] talks to the device over `nusb` (pure-Rust USB — the choice
//! that drops the libusb C dependency and the pkg-config build pain). It also
//! centralizes the macOS lesson: expose the link speed so the caller can
//! diagnose the SuperSpeed/U1-U2 failure instead of returning a bare I/O error.
//!
//! [`MockTransport`] replays capture fixtures, turning the T76 protocol into
//! hardware-free unit tests.
//!
//! `nusb` is async at heart, but every call site here uses its blocking
//! surface (`MaybeFuture::wait` and `Endpoint::transfer_blocking`), so the
//! [`Transport`] trait stays synchronous and no executor leaks upward.
#![forbid(unsafe_code)]

use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::time::Duration;

use minipro_core::error::{Error, Result};
use minipro_core::transport::{Ep, LinkSpeed, Transport};
use nusb::transfer::{Buffer, Bulk, In, Out};
use nusb::{Endpoint, MaybeFuture, Speed};

/// XGecu T76 vendor id.
pub const T76_VID: u16 = 0xA466;
/// XGecu T76 product id.
pub const T76_PID: u16 = 0x1A86;

/// The TL866II+, T48, and T56 all enumerate as one USB id (`0xA466:0x0A53`);
/// the concrete model is read from the system-info device-type byte, not the
/// PID. Opening this id covers all three.
pub const TL866II_VID: u16 = 0xA466;
/// Shared TL866II+/T48/T56 product id.
pub const TL866II_PID: u16 = 0x0A53;

/// USB ids this port can open, in probe order: the T76, then the shared
/// TL866II+/T48/T56 id. [`UsbTransport::open_any`] tries each; `detect`
/// resolves the concrete driver afterward from the system-info byte.
pub const KNOWN_IDS: &[(u16, u16)] = &[(T76_VID, T76_PID), (TL866II_VID, TL866II_PID)];

/// The T76 bulk command endpoints (vendor protocol: commands OUT, responses IN).
const EP_CMD_OUT: u8 = 0x01;
const EP_CMD_IN: u8 = 0x81;

/// Vendor `MP_USBTIMEOUT`: every OUT transfer, and IN transfers on the payload
/// and status endpoints, complete within seconds or not at all.
const SHORT_TIMEOUT: Duration = Duration::from_millis(5_000);
/// Vendor `MP_USB_READ_TIMEOUT`: command responses on EP 0x81 can lag by a full
/// chip-erase (minutes), so that endpoint alone gets the long deadline.
const CMD_READ_TIMEOUT: Duration = Duration::from_millis(360_000);

/// Interface-claim retry (vendor: 5 attempts, 200 ms apart — the pipes need a
/// moment to settle after the T76 config-cycle re-arm below).
const CLAIM_ATTEMPTS: u32 = 5;
const CLAIM_RETRY_DELAY: Duration = Duration::from_millis(200);

/// Re-enumeration poll after a device reset. Measured worst case on a live T76
/// is ~50 ms from the reset returning to open-and-claimable, so a 10 ms poll
/// finds the device on its first or second look. See the `reset_*` hardware
/// tests, which record the numbers this is derived from.
const REENUMERATE_POLL: Duration = Duration::from_millis(10);
/// Overall budget for a device to come back after a reset. Far above the
/// measured worst case on purpose: a slow or loaded host should still recover.
const REENUMERATE_TIMEOUT: Duration = Duration::from_secs(5);

/// Real USB transport over `nusb`.
pub struct UsbTransport {
    device: nusb::Device,
    /// Held purely to keep interface 0 claimed for the endpoints' lifetime.
    /// `None` only inside [`Transport::reset`], between releasing the claim and
    /// re-claiming after re-enumeration: a device cannot be reset while any of
    /// its interfaces is claimed.
    interface: Option<nusb::Interface>,
    /// Bulk OUT endpoints, claimed lazily and cached (0x01 command, 0x05 payload…).
    out_eps: HashMap<u8, Endpoint<Bulk, Out>>,
    /// Bulk IN endpoints, claimed lazily and cached (0x81 command, 0x82 payload…).
    in_eps: HashMap<u8, Endpoint<Bulk, In>>,
    link: LinkSpeed,
    vid: u16,
    pid: u16,
    /// USB serial of the unit this transport opened, when the device reports
    /// one. Load-bearing across [`Transport::reset`]: it is what stops the
    /// re-arm from binding to a different programmer. `None` when the device
    /// exposes no serial descriptor, which reset handles by refusing to guess.
    serial: Option<String>,
}

/// Which physical device [`open_device`] should pick when several share a
/// `vid:pid`.
enum Select<'a> {
    /// First match — correct for an initial open, where any attached
    /// programmer of the right model will do.
    Any,
    /// Exactly the unit with this USB serial.
    Serial(&'a str),
    /// The only match, refusing if there is more than one. Used when re-arming
    /// a device that reports no serial: continuing would mean guessing.
    Sole,
}

impl UsbTransport {
    /// Open the T76 (`0xA466:0x1A86`) and claim interface 0.
    ///
    /// Enumerates with `nusb`, opens the first matching device, applies the
    /// T76 endpoint-arming workaround, claims interface 0 (with retry), claims
    /// the bulk command endpoints 0x01 OUT / 0x81 IN, and records the
    /// negotiated link speed.
    pub fn open(vid: u16, pid: u16) -> Result<Self> {
        let (device, interface, link, serial) = open_device(vid, pid, Select::Any)?;
        let mut tx = UsbTransport {
            device,
            interface: Some(interface),
            out_eps: HashMap::new(),
            in_eps: HashMap::new(),
            link,
            vid,
            pid,
            serial,
        };
        // Claim the command pair eagerly: if 0x01/0x81 are missing or not bulk
        // this is the wrong device, and we want to fail in open, not on the
        // first command.
        tx.out_ep(EP_CMD_OUT)?;
        tx.in_ep(EP_CMD_IN)?;
        Ok(tx)
    }

    /// Open the first present known programmer, probing [`KNOWN_IDS`] in order.
    /// The concrete model (T56/T48/T76, or an unsupported TL866II+) is resolved
    /// later by `detect` from the system-info byte — all of T48/T56/TL866II+
    /// share one USB id, so there is nothing to choose between here.
    pub fn open_any() -> Result<Self> {
        let mut last: Option<Error> = None;
        for &(vid, pid) in KNOWN_IDS {
            match Self::open(vid, pid) {
                Ok(tx) => return Ok(tx),
                Err(e) => last = Some(e),
            }
        }
        Err(no_device_error(last))
    }

    /// On macOS + SuperSpeed the T76 bulk path fails; surface a clear diagnostic.
    pub fn check_link(&self) -> Result<()> {
        superspeed_diagnostic(cfg!(target_os = "macos"), self.link)
    }

    /// Get (claiming and caching on first use) a bulk OUT endpoint.
    fn out_ep(&mut self, addr: u8) -> Result<&mut Endpoint<Bulk, Out>> {
        if addr & 0x80 != 0 {
            return Err(Error::Usb(format!(
                "endpoint 0x{addr:02x} is not an OUT endpoint"
            )));
        }
        match self.out_eps.entry(addr) {
            Entry::Occupied(e) => Ok(e.into_mut()),
            Entry::Vacant(v) => {
                let ep = self
                    .interface
                    .as_ref()
                    .ok_or_else(no_interface)?
                    .endpoint::<Bulk, Out>(addr)
                    .map_err(|e| Error::Usb(format!("claim bulk OUT 0x{addr:02x}: {e}")))?;
                Ok(v.insert(ep))
            }
        }
    }

    /// Get (claiming and caching on first use) a bulk IN endpoint.
    fn in_ep(&mut self, addr: u8) -> Result<&mut Endpoint<Bulk, In>> {
        if addr & 0x80 == 0 {
            return Err(Error::Usb(format!(
                "endpoint 0x{addr:02x} is not an IN endpoint"
            )));
        }
        match self.in_eps.entry(addr) {
            Entry::Occupied(e) => Ok(e.into_mut()),
            Entry::Vacant(v) => {
                let ep = self
                    .interface
                    .as_ref()
                    .ok_or_else(no_interface)?
                    .endpoint::<Bulk, In>(addr)
                    .map_err(|e| Error::Usb(format!("claim bulk IN 0x{addr:02x}: {e}")))?;
                Ok(v.insert(ep))
            }
        }
    }
}

impl Transport for UsbTransport {
    fn send(&mut self, ep: Ep, data: &[u8]) -> Result<()> {
        tracing::trace!(
            ep = format_args!("{:02x}", ep.0),
            len = data.len(),
            "usb out"
        );
        let endpoint = self.out_ep(ep.0)?;
        let mut buf = Buffer::new(data.len());
        buf.extend_from_slice(data);
        let completion = endpoint.transfer_blocking(buf, SHORT_TIMEOUT);
        completion
            .status
            .map_err(|e| Error::Usb(format!("bulk OUT 0x{:02x}: {e}", ep.0)))?;
        if completion.actual_len != data.len() {
            return Err(Error::Usb(format!(
                "bulk OUT 0x{:02x}: short write {}/{}",
                ep.0,
                completion.actual_len,
                data.len()
            )));
        }
        Ok(())
    }

    fn recv(&mut self, ep: Ep, len: usize) -> Result<Vec<u8>> {
        tracing::trace!(ep = format_args!("{:02x}", ep.0), len, "usb in");
        if len == 0 {
            return Ok(Vec::new());
        }
        // Command responses (EP 0x81) may take a full chip operation to arrive;
        // payload/status endpoints answer within seconds (vendor timeouts).
        let timeout = if ep.0 == EP_CMD_IN {
            CMD_READ_TIMEOUT
        } else {
            SHORT_TIMEOUT
        };
        let endpoint = self.in_ep(ep.0)?;
        // IN transfers must request a nonzero multiple of the max packet size
        // (the libusb-overflow rule: short reads are rounded up to 64 bytes).
        let mps = endpoint.max_packet_size();
        let request = len.div_ceil(mps) * mps;
        let completion = endpoint.transfer_blocking(Buffer::new(request), timeout);
        completion
            .status
            .map_err(|e| Error::Usb(format!("bulk IN 0x{:02x}: {e}", ep.0)))?;
        if completion.actual_len < len {
            return Err(Error::Usb(format!(
                "bulk IN 0x{:02x}: short read {}/{len}",
                ep.0, completion.actual_len
            )));
        }
        let mut data = completion.buffer.into_vec();
        data.truncate(len);
        Ok(data)
    }

    fn link_speed(&self) -> LinkSpeed {
        self.link
    }

    fn reset(&mut self) -> Result<()> {
        // A reset re-enumerates the device and invalidates every handle, so
        // drop the endpoints *and* release interface 0 first: a device with a
        // claimed interface cannot be reset ("cannot perform this operation
        // while interfaces are claimed"), which made this path fail outright.
        // Order matters — the endpoints borrow their claim from the interface.
        self.out_eps.clear();
        self.in_eps.clear();
        self.interface = None;
        self.device
            .reset()
            .wait()
            .map_err(|e| Error::Usb(format!("device reset: {e}")))?;

        // Cadence measured on a live T76 (macOS, High Speed, 5 rounds): the
        // reset call itself blocks ~95 ms, the device reappears 17-31 ms later,
        // and is open-and-claimable 27-50 ms after that — first try, every
        // time. Polling promptly costs nothing when the device is slower, and
        // `open_device` already absorbs a transient claim failure internally.
        // The deadline stays generous for loaded or slower hosts.
        // Re-arm *this* unit, not whichever answers first. Matching on vid:pid
        // alone is safe with one programmer attached and silently wrong with
        // two: the transport would carry on against a different device, so a
        // write would land in the wrong chip and the verify read would go to
        // that same wrong chip and pass. Cloned up front because the match arm
        // needs `&mut self`.
        let want_serial = self.serial.clone();
        let deadline = std::time::Instant::now() + REENUMERATE_TIMEOUT;
        loop {
            std::thread::sleep(REENUMERATE_POLL);
            let sel = match want_serial.as_deref() {
                Some(s) => Select::Serial(s),
                None => Select::Sole,
            };
            match open_device(self.vid, self.pid, sel) {
                Ok((device, interface, link, serial)) => {
                    self.device = device;
                    self.interface = Some(interface);
                    self.link = link;
                    self.serial = serial;
                    self.out_ep(EP_CMD_OUT)?;
                    self.in_ep(EP_CMD_IN)?;
                    return Ok(());
                }
                // Report why it never came back, not a generic timeout.
                Err(e) if std::time::Instant::now() >= deadline => return Err(e),
                Err(_) => {}
            }
        }
    }
}

/// The transport lost its interface claim — only reachable if a [`Transport::reset`]
/// released it and then failed to re-establish it.
fn no_interface() -> Error {
    Error::Usb("usb interface is not claimed (a device reset did not complete)".into())
}

/// Choose the error [`UsbTransport::open_any`] returns after every id failed.
/// A genuine open/claim failure (device present but unusable) is surfaced
/// verbatim so permission/driver issues stay debuggable; a plain "not found"
/// collapses to a clean "nothing connected" that lists what was tried.
fn no_device_error(last: Option<Error>) -> Error {
    match last {
        Some(e) if !e.to_string().contains("no programmer found") => e,
        _ => {
            let ids: Vec<String> = KNOWN_IDS
                .iter()
                .map(|(v, p)| format!("{v:04x}:{p:04x}"))
                .collect();
            Error::Usb(format!(
                "no known programmer connected (tried {})",
                ids.join(", ")
            ))
        }
    }
}

/// Enumerate, open, arm (T76 only), and claim interface 0 of `vid:pid`,
/// choosing between multiple attached units per `sel`. Also returns the chosen
/// unit's USB serial, so a later reset can insist on the same one.
fn open_device(
    vid: u16,
    pid: u16,
    sel: Select<'_>,
) -> Result<(nusb::Device, nusb::Interface, LinkSpeed, Option<String>)> {
    let devices = nusb::list_devices()
        .wait()
        .map_err(|e| Error::Usb(format!("enumerate: {e}")))?;
    let mut found: Vec<_> = devices
        .into_iter()
        .filter(|d| d.vendor_id() == vid && d.product_id() == pid)
        .collect();
    // The "no programmer found" wording is a contract with `no_device_error`,
    // which keys on it to decide between a clean listing and a verbatim error.
    let absent = || Error::Usb(format!("no programmer found ({vid:04x}:{pid:04x})"));
    let info = match sel {
        Select::Any => found.into_iter().next().ok_or_else(absent)?,
        Select::Serial(want) => found
            .into_iter()
            .find(|d| d.serial_number().is_some_and(|s| s == want))
            .ok_or_else(|| {
                Error::Usb(format!(
                    "programmer {want} did not come back after its reset; \
                     refusing to continue against a different unit"
                ))
            })?,
        Select::Sole => {
            if found.len() > 1 {
                return Err(Error::Usb(format!(
                    "{} programmers match {vid:04x}:{pid:04x} and this one reports no USB \
                     serial, so the unit that was reset cannot be told apart from the others; \
                     leave one attached, or reopen explicitly",
                    found.len()
                )));
            }
            found.pop().ok_or_else(absent)?
        }
    };
    let serial = info.serial_number().map(|s| s.to_string());
    let link = link_speed_from(info.speed());
    let device = info
        .open()
        .wait()
        .map_err(|e| Error::Usb(format!("open {vid:04x}:{pid:04x}: {e}")))?;

    // T76 (0xA466:0x1A86) on macOS: at USB 3.0 SuperSpeed the bulk endpoints
    // often come up unresponsive (kIOReturnNotResponding on the first OUT)
    // even though control transfers and interface-claim succeed. Cycling the
    // configuration (0 -> 1) re-arms the endpoints. Harmless on other OSes and
    // guarded to the T76 so it can't disturb the TL866/T48/T56 devices.
    // The set_configuration return is ignored — it's advisory, and the re-arm
    // is a best-effort workaround.
    if vid == T76_VID && pid == T76_PID {
        let _ = device.set_configuration(0).wait();
        let _ = device.set_configuration(1).wait();
    }

    // Claim with a short retry: the pipes need a moment to settle after the
    // reconfigure above, and macOS occasionally returns a transient error on
    // the first claim.
    let mut interface = None;
    let mut last_err = None;
    for attempt in 0..CLAIM_ATTEMPTS {
        if attempt > 0 {
            std::thread::sleep(CLAIM_RETRY_DELAY);
        }
        match device.claim_interface(0).wait() {
            Ok(iface) => {
                interface = Some(iface);
                break;
            }
            Err(e) => last_err = Some(e),
        }
    }
    let interface = interface.ok_or_else(|| {
        let detail = last_err.map(|e| e.to_string()).unwrap_or_default();
        Error::Usb(format!("claim interface 0: {detail}"))
    })?;

    Ok((device, interface, link, serial))
}

/// Map nusb's negotiated speed onto the trait's three-way [`LinkSpeed`].
///
/// `None` (platform could not report a speed) maps to `High`: the practical
/// default for this hardware, and one that never false-triggers the macOS
/// SuperSpeed diagnostic.
fn link_speed_from(speed: Option<Speed>) -> LinkSpeed {
    match speed {
        Some(Speed::Low | Speed::Full) => LinkSpeed::Full,
        Some(Speed::High) | None => LinkSpeed::High,
        // Super, SuperPlus, and any future (faster) variants of the
        // non-exhaustive enum: all take the SuperSpeed bulk path.
        Some(_) => LinkSpeed::Super,
    }
}

/// The macOS-lesson check behind [`UsbTransport::check_link`], parameterized on
/// the OS so it is unit-testable on any host.
fn superspeed_diagnostic(is_macos: bool, link: LinkSpeed) -> Result<()> {
    if is_macos && link == LinkSpeed::Super {
        return Err(Error::Usb(
            "T76 on macOS SuperSpeed: bulk transfers fail; use a USB 2.0 cable".into(),
        ));
    }
    Ok(())
}

/// A record/replay transport for protocol unit tests. Each `send` must match the
/// next expected packet; each `recv` returns the next canned response.
pub struct MockTransport {
    script: Vec<(Vec<u8>, Vec<u8>)>, // (expected out, canned in)
    pos: usize,
}

impl MockTransport {
    /// Build from a captured `(out, in)` script (e.g. from a vendor USB capture).
    pub fn from_script(script: Vec<(Vec<u8>, Vec<u8>)>) -> Self {
        MockTransport { script, pos: 0 }
    }
}

impl Transport for MockTransport {
    fn send(&mut self, _ep: Ep, data: &[u8]) -> Result<()> {
        match self.script.get(self.pos) {
            Some((expected, _)) if expected == data => Ok(()),
            Some(_) => Err(Error::Protocol),
            None => Err(Error::Protocol),
        }
    }
    fn recv(&mut self, _ep: Ep, _len: usize) -> Result<Vec<u8>> {
        let resp = self.script.get(self.pos).map(|(_, r)| r.clone());
        self.pos += 1;
        resp.ok_or(Error::Protocol)
    }
    fn link_speed(&self) -> LinkSpeed {
        LinkSpeed::High
    }
    fn reset(&mut self) -> Result<()> {
        self.pos = 0;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use minipro_core::transport::{command, Ep};

    // Proves the testability story: a driver's protocol can be exercised with no
    // hardware, and the must-drain `Pending` guard forces the response read.
    #[test]
    fn command_roundtrip_over_mock() {
        let script = vec![(vec![0x3e, 0, 0, 0], vec![0xaa; 32])];
        let mut tx = MockTransport::from_script(script);
        let pending = command(&mut tx, Ep(0x01), Ep(0x81), &[0x3e, 0, 0, 0], 32).unwrap();
        let resp = pending.read().unwrap(); // dropping without read would warn
        assert_eq!(resp.len(), 32);
        assert_eq!(resp[0], 0xaa);
    }

    /// Measures what [`UsbTransport::reset`] actually has to wait for, so the
    /// retry cadence can be set from data instead of copied from the vendor.
    ///
    /// Needs a connected T76:
    /// ```text
    /// cargo test -p minipro-usb -- --ignored --nocapture reset_reenumeration
    /// ```
    ///
    /// Enumeration and readiness are timed separately: "visible in the device
    /// list" and "open + interface claimed" are different events, and only the
    /// second one lets a command go out.
    #[test]
    #[ignore = "requires a connected T76"]
    fn reset_reenumeration_timing() {
        use std::time::Instant;
        const POLL: Duration = Duration::from_millis(2);
        const GIVE_UP: Duration = Duration::from_secs(15);
        const ROUNDS: usize = 5;

        let mut worst_ready = Duration::ZERO;
        for round in 1..=ROUNDS {
            let (device, interface, _, serial) =
                open_device(T76_VID, T76_PID, Select::Any).expect("a T76 must be connected");
            drop(interface);
            // Whether the T76 exposes a serial decides how `reset` re-arms:
            // by serial if present, else by insisting it is the sole match.
            if round == 1 {
                println!("usb serial descriptor: {serial:?}");
            }

            let t = Instant::now();
            device.reset().wait().expect("device reset");
            let reset_call = t.elapsed();
            drop(device); // the handle is invalid past reset

            // Phase 1: back in the enumeration list.
            let t = Instant::now();
            let enumerated = loop {
                let seen = nusb::list_devices()
                    .wait()
                    .map(|ds| {
                        ds.into_iter()
                            .any(|d| d.vendor_id() == T76_VID && d.product_id() == T76_PID)
                    })
                    .unwrap_or(false);
                if seen {
                    break t.elapsed();
                }
                assert!(t.elapsed() < GIVE_UP, "never re-enumerated");
                std::thread::sleep(POLL);
            };

            // Phase 2: actually usable — this is the event reset() needs.
            let mut attempts = 0usize;
            let ready = loop {
                attempts += 1;
                if open_device(T76_VID, T76_PID, Select::Any).is_ok() {
                    break t.elapsed();
                }
                assert!(t.elapsed() < GIVE_UP, "never became claimable");
                std::thread::sleep(POLL);
            };

            worst_ready = worst_ready.max(ready);
            println!(
                "round {round}: reset()={reset_call:?}  enumerated={enumerated:?}  \
                 ready={ready:?}  open_device attempts={attempts}"
            );
        }
        println!("worst ready: {worst_ready:?}");
    }

    /// End-to-end cost of a real [`UsbTransport::reset`], which is what the
    /// retry cadence actually buys or wastes. Companion to
    /// `reset_reenumeration_timing`, which decomposes the wait.
    #[test]
    #[ignore = "requires a connected T76"]
    fn reset_end_to_end_cost() {
        use std::time::Instant;
        let mut tx = UsbTransport::open(T76_VID, T76_PID).expect("a T76 must be connected");
        let mut worst = Duration::ZERO;
        for round in 1..=5 {
            let t = Instant::now();
            tx.reset().expect("reset");
            let dt = t.elapsed();
            worst = worst.max(dt);
            println!("round {round}: UsbTransport::reset() = {dt:?}");
        }
        println!("worst reset(): {worst:?}");
    }

    /// A live bulk round-trip, before and after a reset.
    ///
    /// System info is the only command this crate can build without reaching
    /// into protocol knowledge — the request is five zero bytes — and it is
    /// read-only: no bitstream upload, no socket voltage. **No chip need be
    /// inserted**, so anyone with a T76 and a cable can run this.
    ///
    /// ```text
    /// cargo test -p minipro-usb -- --ignored --nocapture live_command
    /// ```
    #[test]
    #[ignore = "requires a connected T76"]
    fn live_command_roundtrip_survives_reset() {
        /// Send the system-info request and drain its reply. Draining is not
        /// optional: an undrained EP81 wedges the T76 until it is replugged.
        fn probe(tx: &mut UsbTransport) -> Vec<u8> {
            let pending = command(tx, Ep(EP_CMD_OUT), Ep(EP_CMD_IN), &[0u8; 5], 64)
                .expect("send system-info request");
            pending.read().expect("drain system-info reply")
        }
        // System-info report layout: [6] = device type, [24..32] = device code.
        const DEVICE_TYPE: usize = 6;
        const T76_DEVICE_TYPE: u8 = 0x08;

        let mut tx = UsbTransport::open(T76_VID, T76_PID).expect("a T76 must be connected");

        let before = probe(&mut tx);
        assert_eq!(before.len(), 64, "system info must return a full report");
        assert_eq!(
            before[DEVICE_TYPE], T76_DEVICE_TYPE,
            "device-type byte must identify a T76"
        );

        tx.reset().expect("reset");

        // The half `reset_end_to_end_cost` cannot establish: the device is not
        // merely re-enumerated and claimable, it still carries bulk traffic.
        let after = probe(&mut tx);
        assert_eq!(
            after[DEVICE_TYPE], T76_DEVICE_TYPE,
            "device stopped answering after a reset"
        );
        // Guards against re-arming onto a *different* unit: `open_device`
        // matches on vid:pid alone, so with two programmers attached the reset
        // could hand back the wrong one.
        assert_eq!(
            &after[24..32],
            &before[24..32],
            "re-armed a different programmer than the one that was reset"
        );
        println!(
            "round-trip ok before and after reset; link = {:?}",
            tx.link_speed()
        );
    }

    #[test]
    fn known_ids_cover_t76_and_shared_family() {
        assert!(KNOWN_IDS.contains(&(T76_VID, T76_PID)));
        assert!(KNOWN_IDS.contains(&(TL866II_VID, TL866II_PID)));
    }

    #[test]
    fn open_any_error_selection() {
        // A genuine claim failure is surfaced verbatim (stays debuggable).
        let real = no_device_error(Some(Error::Usb("claim interface 0: access denied".into())));
        assert!(real.to_string().contains("access denied"));

        // A plain "not found" collapses to a clean listing of every tried id.
        let none = no_device_error(Some(Error::Usb("no programmer found (a466:0a53)".into())));
        let msg = none.to_string();
        assert!(msg.contains("no known programmer connected"));
        assert!(msg.contains("a466:1a86") && msg.contains("a466:0a53"));
    }

    #[test]
    fn mock_rejects_wrong_packet() {
        let mut tx = MockTransport::from_script(vec![(vec![0x01], vec![0x00])]);
        assert!(tx.send(Ep(0x01), &[0x99]).is_err()); // desync -> Protocol error
    }

    // check_link delegates to superspeed_diagnostic(cfg!(macos), link); the
    // helper is what makes the macOS+Super rejection testable on every host
    // without a live device.
    #[test]
    fn check_link_rejects_macos_superspeed() {
        let err = superspeed_diagnostic(true, LinkSpeed::Super).unwrap_err();
        assert_eq!(err.code(), "usb");
        assert!(err.to_string().contains("SuperSpeed"));
        assert!(err.to_string().contains("USB 2.0"));
        // The typed error carries the hard-won remediation hint.
        assert_eq!(
            err.hint(),
            Some("on macOS + T76, connect via a USB 2.0 cable to force High Speed")
        );
    }

    #[test]
    fn check_link_accepts_other_combinations() {
        assert!(superspeed_diagnostic(true, LinkSpeed::High).is_ok());
        assert!(superspeed_diagnostic(true, LinkSpeed::Full).is_ok());
        assert!(superspeed_diagnostic(false, LinkSpeed::Super).is_ok());
        assert!(superspeed_diagnostic(false, LinkSpeed::High).is_ok());
    }

    #[test]
    fn link_speed_mapping_matches_trait_serialization() {
        assert_eq!(link_speed_from(Some(Speed::Low)), LinkSpeed::Full);
        assert_eq!(link_speed_from(Some(Speed::Full)), LinkSpeed::Full);
        assert_eq!(link_speed_from(Some(Speed::High)), LinkSpeed::High);
        assert_eq!(link_speed_from(Some(Speed::Super)), LinkSpeed::Super);
        assert_eq!(link_speed_from(Some(Speed::SuperPlus)), LinkSpeed::Super);
        // Unknown never false-triggers the macOS SuperSpeed diagnostic.
        assert_eq!(link_speed_from(None), LinkSpeed::High);
    }

    #[test]
    fn open_fails_cleanly_with_no_device() {
        // No T76 is attached in CI; open must fail with a typed USB error
        // (enumeration succeeding but finding no match), never panic. The
        // live open/claim/transfer path is covered by the `#[ignore]`d
        // hardware tests above, which need a real device.
        match UsbTransport::open(T76_VID, T76_PID) {
            Err(e) => assert_eq!(e.code(), "usb"),
            // If a real T76 *is* plugged in, open should have succeeded and
            // the command endpoints must have been claimed.
            Ok(tx) => {
                let _ = tx.link_speed();
            }
        }
    }
}
