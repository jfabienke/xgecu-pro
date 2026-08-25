//! zifmap -- map the T76's ZIF socket from inside the socket.
//!
//! The `t76_beacon` bitstream drives all 48 ZIF pins at once, each one
//! continuously transmitting its own name ("Z12\r\n") at 115200 8N1. This
//! firmware sits in the socket, reads the pins it is wired to, and reports
//! which name arrived on which physical socket position.
//!
//! The key property that makes this cheap: every beacon pin transmits in
//! lockstep from one shared sequencer, so this is not 26 independent UART
//! receivers. It is one sampler plus a software decode per channel.
//!
//! Wiring: socket position `SOCKET_BASE + i` -> `CHANNELS[i]`, through a 10k
//! series resistor per line. The resistor is not optional: the ZIF rails are
//! FPGA-switched and their polarity is still unverified, so 10k is what keeps
//! a rail misfire at ~1 mA into the clamp instead of a dead board.
//!
//! Ground comes from USB, shared with the T76 through the host. Never take
//! ground from a socket pin -- on this board every exposed "GND" is a switch
//! output, not a ground.

#![no_std]
#![no_main]

use cortex_m_rt::entry;
use defmt_rtt as _;
use panic_probe as _;

use core::fmt::Write as _;
use hal::dma::{single_buffer, DMAExt};
use hal::pio::{Buffers, PIOExt, ShiftDirection};
use hal::Clock;
use rp2040_hal as hal;
use rp2040_hal::pac;
use static_cell::StaticCell;
use usb_device::bus::UsbBusAllocator;
use usb_device::device::{StringDescriptors, UsbDeviceBuilder, UsbDeviceState, UsbVidPid};
use usbd_serial::SerialPort;

#[link_section = ".boot2"]
#[used]
pub static BOOT2: [u8; 256] = rp2040_boot2::BOOT_LOADER_W25Q080;

const XTAL_HZ: u32 = 12_000_000;

/// The Pico's LEFT header row, physical pins 1..20, in board order.
///
/// Only this row goes into the socket: a Pico is 0.7 in between rows and the
/// ZIF is 0.6 in, so it cannot seat on both. One row at a time also keeps the
/// right-hand row out of the socket, which matters -- physical pins 36..40 are
/// 3V3(OUT), 3V3_EN, GND, VSYS and VBUS, and VBUS on a socket contact is 5 V
/// going somewhere it was never meant to go.
///
/// `None` is a ground pin. The Pico's left row carries four of them, at
/// physical pins 3, 8, 13 and 18, and they SHORT those socket contacts to
/// ground. Nothing can be mapped there, and if a rail ever comes up on one of
/// those positions it is a supply-to-ground short through the socket driver.
const LEFT_ROW: [Option<u8>; 20] = [
    Some(0),  // pin 1  -- GP0, the end nearest USB
    Some(1),  // pin 2
    None,     // pin 3  -- GND
    Some(2),  // pin 4
    Some(3),  // pin 5
    Some(4),  // pin 6
    Some(5),  // pin 7
    None,     // pin 8  -- GND
    Some(6),  // pin 9
    Some(7),  // pin 10
    Some(8),  // pin 11
    Some(9),  // pin 12
    None,     // pin 13 -- GND
    Some(10), // pin 14
    Some(11), // pin 15
    Some(12), // pin 16
    Some(13), // pin 17
    None,     // pin 18 -- GND
    Some(14), // pin 19
    Some(15), // pin 20, the end furthest from USB
];

/// The step-index channel. During a rail sweep the FPGA drives no socket pins
/// at all, so it announces which experiment it is running on one ISP header pin
/// instead. GP16 is on the Pico's right-hand row -- out of the socket and
/// otherwise unused -- so one jumper tags every reading with the state that
/// produced it. The sweep frames it exactly like a beacon name ("S00\r\n"), so
/// the existing decoder handles it with no change to bit timing.
const STEP_CH: u8 = 16;

/// Which way round the board sits, for the report header only. With the USB
/// connector pointing away from the ZIF latch, physical pin 1 is the end
/// furthest from the latch.
const USB_AWAY_FROM_LATCH: bool = true;

/// 32768 samples at ~1.152 MSa/s is 28.4 ms.
///
/// Sized by the census frame, not the beacon: that frame is 235 bytes, which at
/// 115200 8N1 takes 20.4 ms. A 14.2 ms window truncated every one of them, and
/// a truncated frame with a valid-looking preamble is worse than no frame --
/// it decodes into plausible nonsense. 28.4 ms guarantees one whole frame
/// lands inside the window wherever it starts.
///
/// 131 KB of the RP2040's 264 KB, which is affordable.
const N_SAMPLES: usize = 32_768;

/// PIO clock divider for a 1.152 MSa/s sample rate from a 125 MHz system
/// clock: 125e6 / 1_152_000 = 108.5069, and 108 + 130/256 = 108.5078.
const CLKDIV_INT: u16 = 108;
const CLKDIV_FRAC: u8 = 130;
const SAMPLE_HZ: u32 = 1_151_990;

/// The beacon's real baud rate, not its nominal one. Its divider is
/// `20_000_000 / 115200 = 173.6`, rounded to 174, so it actually transmits at
/// 20e6/174 = 114_942.5 baud. Decoding against 115200 would drift 2.2% of a
/// bit by the stop bit -- harmless here, but there is no reason to introduce
/// error we can just as easily not introduce.
const BEACON_BAUD: u32 = 20_000_000 / 174;

/// Samples per bit in Q8 fixed point.
const SPB_Q8: u32 = ((SAMPLE_HZ as u64 * 256) / BEACON_BAUD as u64) as u32;

static BUF: StaticCell<[u32; N_SAMPLES]> = StaticCell::new();
static USB_BUS: StaticCell<UsbBusAllocator<hal::usb::UsbBus>> = StaticCell::new();

/// What one channel's sample stream decoded to.
#[derive(Clone, Copy)]
struct Chan {
    /// Bytes that framed cleanly (valid start and stop bit).
    bytes_ok: u16,
    /// Falling edges that looked like a start bit and then did not frame.
    framing_err: u16,
    /// Complete "Xnn\r\n" beacon frames.
    name_hits: u16,
    name: [u8; 3],
    /// Fraction of samples high, in percent -- separates a live line from one
    /// stuck at a rail.
    duty_pct: u32,
    /// Shortest run of identical samples, which for a UART is one bit time.
    /// Reported so a baud mismatch shows up as a number instead of as silence.
    min_run: u16,
}

impl Chan {
    const EMPTY: Chan = Chan {
        bytes_ok: 0,
        framing_err: 0,
        name_hits: 0,
        name: [0; 3],
        duty_pct: 0,
        min_run: u16::MAX,
    };
}

#[inline(always)]
fn bit_at(buf: &[u32], ch: u8, i: usize) -> u8 {
    ((buf[i] >> ch) & 1) as u8
}

/// Decode one channel out of the parallel capture.
fn decode(buf: &[u32], ch: u8) -> Chan {
    let n = buf.len();
    let mut c = Chan::EMPTY;

    // Level statistics and shortest run, computed in one sweep. The first and
    // last runs are clipped by the capture window, so they are not measurements
    // of anything and are excluded from `min_run`.
    let mut ones: u32 = 0;
    let mut run: u16 = 0;
    let mut prev = bit_at(buf, ch, 0);
    let mut first_run_done = false;
    for i in 0..n {
        let s = bit_at(buf, ch, i);
        ones += s as u32;
        if s == prev {
            run = run.saturating_add(1);
        } else {
            if first_run_done && run < c.min_run {
                c.min_run = run;
            }
            first_run_done = true;
            run = 1;
            prev = s;
        }
    }
    c.duty_pct = ones * 100 / n as u32;
    if c.min_run == u16::MAX {
        c.min_run = 0; // no transition at all: the line never moved
    }

    // UART decode. Sample each bit at its centre, using Q8 positions so the
    // non-integer samples-per-bit does not accumulate rounding error across
    // the frame.
    let half = SPB_Q8 / 2;
    let mut recent = [0u8; 5];
    let mut i = 1usize;
    while i < n {
        if !(bit_at(buf, ch, i - 1) == 1 && bit_at(buf, ch, i) == 0) {
            i += 1;
            continue;
        }
        let base = (i as u32) << 8;
        let pos = |k: u32| -> usize { ((base + half + k * SPB_Q8) >> 8) as usize };
        if pos(9) >= n {
            break;
        }
        // Start bit still low at its centre, stop bit high at its centre.
        if bit_at(buf, ch, pos(0)) != 0 || bit_at(buf, ch, pos(9)) != 1 {
            c.framing_err = c.framing_err.saturating_add(1);
            i += 1;
            continue;
        }
        let mut b = 0u8;
        for k in 0..8u32 {
            b |= bit_at(buf, ch, pos(k + 1)) << k; // 8N1 is LSB first
        }
        c.bytes_ok = c.bytes_ok.saturating_add(1);

        recent.rotate_left(1);
        recent[4] = b;
        if recent[3] == b'\r'
            && recent[4] == b'\n'
            && recent[0].is_ascii_uppercase()
            && recent[1].is_ascii_digit()
            && recent[2].is_ascii_digit()
        {
            c.name_hits = c.name_hits.saturating_add(1);
            c.name = [recent[0], recent[1], recent[2]];
        }

        i = pos(9) + 1;
    }
    c
}

/// A line buffer that goes out over USB CDC, and over RTT as well when a probe
/// happens to be attached.
///
/// USB is the primary path deliberately. SWD died four times in one bench
/// session here, every time the board was handled -- the leads on the Pico's
/// SWD pads are the fragile part of this rig. The Pico's own USB never once
/// failed, and UF2 flashing does not need a probe either, so nothing about
/// this tool depends on a debug probe any more.
struct Line {
    buf: heapless::String<192>,
}

impl Line {
    const fn new() -> Self {
        Line { buf: heapless::String::new() }
    }
    fn clear(&mut self) {
        self.buf.clear();
    }
    fn as_bytes(&self) -> &[u8] {
        self.buf.as_bytes()
    }
}

impl core::fmt::Write for Line {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        // Truncation beats panicking in a bench instrument; the width is chosen
        // so no line this tool emits can reach it.
        let _ = self.buf.push_str(s);
        Ok(())
    }
}

/// Everything the reporter needs to emit a line, kept together so `report`
/// does not take five arguments.
struct Out<'a, 'b> {
    serial: &'a mut SerialPort<'b, hal::usb::UsbBus>,
    usb: &'a mut usb_device::device::UsbDevice<'b, hal::usb::UsbBus>,
}

impl Out<'_, '_> {
    /// Write one line, pumping the USB stack while the host drains it.
    ///
    /// Bounded rather than blocking: if nothing is listening on the CDC port
    /// the FIFO fills and stays full, and an unbounded retry here would wedge
    /// the capture loop forever waiting for a reader that may never arrive.
    fn line(&mut self, l: &Line) {
        // With no host attached the CDC endpoint never drains, so spinning the
        // full retry budget on every line would make each report take seconds.
        // When the device is not Configured there is no reader by definition:
        // pump the stack once so enumeration can still progress, and let RTT
        // carry the output.
        if self.usb.state() != UsbDeviceState::Configured {
            self.usb.poll(&mut [self.serial]);
            defmt::info!("{=str}", l.buf.as_str());
            return;
        }
        let mut data = l.as_bytes();
        let mut spins = 0u32;
        while !data.is_empty() && spins < 200_000 {
            self.usb.poll(&mut [self.serial]);
            match self.serial.write(data) {
                Ok(n) if n > 0 => {
                    data = &data[n..];
                    spins = 0;
                }
                _ => spins += 1,
            }
        }
        let mut spins = 0u32;
        while !b"\r\n".is_empty() && spins < 20_000 {
            self.usb.poll(&mut [self.serial]);
            if self.serial.write(b"\r\n").is_ok() {
                break;
            }
            spins += 1;
        }
        defmt::info!("{=str}", l.buf.as_str());
    }

    /// Service USB for roughly `ms` milliseconds without blocking it.
    fn idle(&mut self, delay: &mut cortex_m::delay::Delay, ms: u32) {
        for _ in 0..ms {
            self.usb.poll(&mut [self.serial]);
            delay.delay_us(1000);
        }
    }
}

/// Prove every channel's input pad works, with no wiring at all.
///
/// The decoder self-test proves the decoder; it says nothing about the pins.
/// This board was once inserted with the wrong row in a driven ZIF socket --
/// VBUS and 3V3_EN on live contacts -- so "is the pad still alive?" is a real
/// question and not a rhetorical one.
///
/// Uses the internal pulls rather than driving: a weak pull cannot damage
/// anything it is wired to, and if an external driver is holding the pin the
/// pull loses, which is itself the answer.
///
///   (1, 0) -> healthy and unconnected: follows its own pull
///   (1, 1) -> healthy, held HIGH by something external
///   (0, 0) -> held LOW by something external, or the pad is dead
fn pad_selftest(delay: &mut cortex_m::delay::Delay, out: &mut Out<'_, '_>) {
    let pads = unsafe { &*pac::PADS_BANK0::ptr() };
    let sio = unsafe { &*pac::SIO::ptr() };
    let mut l = Line::new();
    let _ = write!(l, "pad test (internal pulls; 1/0 = healthy, 1/1 = driven high, 0/0 = held low or dead)");
    out.line(&l);

    let mut healthy = 0u32;
    let mut suspect = 0u32;
    for (i, slot) in LEFT_ROW.iter().enumerate() {
        let Some(ch) = *slot else { continue };
        pads.gpio(ch as usize).modify(|_, w| w.pue().set_bit().pde().clear_bit());
        delay.delay_us(2000);
        let hi = (sio.gpio_in().read().bits() >> ch) & 1;
        pads.gpio(ch as usize).modify(|_, w| w.pue().clear_bit().pde().set_bit());
        delay.delay_us(2000);
        let lo = (sio.gpio_in().read().bits() >> ch) & 1;

        let verdict = match (hi, lo) {
            (1, 0) => { healthy += 1; "ok (follows its pull)" }
            (1, 1) => { healthy += 1; "ok, held HIGH externally" }
            (0, 0) => { suspect += 1; "HELD LOW or DEAD PAD" }
            _ => { suspect += 1; "inverted -- impossible, suspect pad" }
        };
        l.clear();
        let _ = write!(l, "   pin {:02}  GP{:02}  pu={} pd={}  {}", i + 1, ch, hi, lo, verdict);
        out.line(&l);
    }
    l.clear();
    let _ = write!(l, "pad test: {} healthy, {} suspect", healthy, suspect);
    out.line(&l);
}

/// Synthesise a beacon capture and decode it, so the decoder is proven before
/// any wire is attached.
///
/// A decoder that reports silence is indistinguishable from a quiet line, and
/// this project has already lost time to an instrument that looked like dead
/// hardware. This makes the difference observable: channel 0 carries a real
/// "Z07\r\n" frame at the exact bit timing the field decode will use, channel
/// 1 idles high, channel 2 sits low. If the decoder cannot recover a frame it
/// generated itself, nothing downstream is worth reading.
fn selftest(buf: &mut [u32; N_SAMPLES], out: &mut Out<'_, '_>) -> bool {
    const MSG: [u8; 5] = [b'Z', b'0', b'7', b'\r', b'\n'];

    // ch0 idle high, ch1 stuck high, ch2 stuck low.
    for w in buf.iter_mut() {
        *w = 0b011;
    }

    let mut pos_q8: u32 = SPB_Q8 * 4; // idle lead-in, so the first edge is real
    let mut byte_i = 0usize;
    let mut frames_written = 0u32;
    loop {
        let frame_end = ((pos_q8 + 10 * SPB_Q8) >> 8) as usize;
        if frame_end + 2 >= N_SAMPLES {
            break;
        }
        let b = MSG[byte_i % MSG.len()];
        byte_i += 1;
        if byte_i % MSG.len() == 0 {
            frames_written += 1;
        }
        for k in 0..10u32 {
            let level = match k {
                0 => 0,                    // start
                9 => 1,                    // stop
                _ => (b >> (k - 1)) & 1,   // data, LSB first
            };
            let from = ((pos_q8 + k * SPB_Q8) >> 8) as usize;
            let to = ((pos_q8 + (k + 1) * SPB_Q8) >> 8) as usize;
            for w in buf[from..to.min(N_SAMPLES)].iter_mut() {
                *w = (*w & !1) | level as u32;
            }
        }
        pos_q8 += 10 * SPB_Q8;
    }

    let live = decode(buf, 0);
    let high = decode(buf, 1);
    let low = decode(buf, 2);

    let name_ok = live.name_hits > 0 && &live.name == b"Z07";
    let clean = live.framing_err == 0;
    let counted = live.name_hits as u32 >= frames_written.saturating_sub(1);
    let high_ok = high.name_hits == 0 && high.duty_pct == 100;
    let low_ok = low.name_hits == 0 && low.duty_pct == 0;
    let ok = name_ok && clean && counted && high_ok && low_ok;

    let mut l = Line::new();
    if ok {
        let _ = write!(
            l,
            "selftest PASS: decoded {} x \"Z07\" of {} synthesised, 0 framing errors",
            live.name_hits, frames_written
        );
    } else {
        let _ = write!(
            l,
            "selftest FAIL: name={} clean={} counted={} idle={} low={} (hits {}/{}, err {}, bytes {})",
            name_ok, clean, counted, high_ok, low_ok,
            live.name_hits, frames_written, live.framing_err, live.bytes_ok
        );
    }
    out.line(&l);
    ok
}

/// Decode one channel and return its raw byte stream, for reading a binary
/// frame rather than a beacon name.
///
/// The census emits a length-prefixed binary frame, not `Xnn\r\n`, so the
/// name decoder counts its bytes and understands none of them. This hands the
/// bytes back so the host can parse the frame itself.
fn decode_raw(buf: &[u32], ch: u8, out: &mut [u8]) -> usize {
    let n = buf.len();
    let half = SPB_Q8 / 2;
    let mut got = 0usize;
    let mut i = 1usize;
    while i < n && got < out.len() {
        if !(bit_at(buf, ch, i - 1) == 1 && bit_at(buf, ch, i) == 0) {
            i += 1;
            continue;
        }
        let base = (i as u32) << 8;
        let pos = |k: u32| -> usize { ((base + half + k * SPB_Q8) >> 8) as usize };
        if pos(9) >= n {
            break;
        }
        if bit_at(buf, ch, pos(0)) != 0 || bit_at(buf, ch, pos(9)) != 1 {
            i += 1;
            continue;
        }
        let mut b = 0u8;
        for k in 0..8u32 {
            b |= bit_at(buf, ch, pos(k + 1)) << k;
        }
        out[got] = b;
        got += 1;
        i = pos(9) + 1;
    }
    got
}

/// Sample one channel under pull-up then pull-down.
///
/// The distinction the rail sweep turns on: with nothing driving a socket pin,
/// the Pico's own pull decides the level, so a floating contact and a contact
/// the board has grounded are indistinguishable under a single pull. Under
/// both, they separate cleanly.
///
///   (1, 0)  follows its own pull -> nothing is driving it: ISOLATED
///   (0, 0)  held low against a pull-up   -> the board is GROUNDING it
///   (1, 1)  held high against a pull-down -> the board is DRIVING it high
fn pull_probe(ch: u8, delay: &mut cortex_m::delay::Delay) -> (u32, u32) {
    let pads = unsafe { &*pac::PADS_BANK0::ptr() };
    let sio = unsafe { &*pac::SIO::ptr() };
    pads.gpio(ch as usize).modify(|_, w| w.pue().set_bit().pde().clear_bit());
    delay.delay_us(1500);
    let hi = (sio.gpio_in().read().bits() >> ch) & 1;
    pads.gpio(ch as usize).modify(|_, w| w.pue().clear_bit().pde().set_bit());
    delay.delay_us(1500);
    let lo = (sio.gpio_in().read().bits() >> ch) & 1;
    (hi, lo)
}

fn report(buf: &[u32], pass: u32, out: &mut Out<'_, '_>, delay: &mut cortex_m::delay::Delay) {
    let mut l = Line::new();

    let _ = write!(
        l,
        "---- pass {} | {} samples @ {} Hz | {} baud | spb_q8 {} ----",
        pass, buf.len(), SAMPLE_HZ, BEACON_BAUD, SPB_Q8
    );
    out.line(&l);

    l.clear();
    let _ = write!(
        l,
        "orientation: USB {} latch -> Pico pin 1 is the {}",
        if USB_AWAY_FROM_LATCH { "away from" } else { "toward" },
        if USB_AWAY_FROM_LATCH { "far end from the latch" } else { "latch end" }
    );
    out.line(&l);

    // The sweep announces its state on GP16. Silent here simply means no sweep
    // is running -- the beacon does not use this channel.
    let step = decode(buf, STEP_CH);

    // Sweep mode gets ONE line per pass, not the full table. A 49-step walk at
    // table width is four thousand lines to read for what is, per step, a
    // single fact: which position went to ground. Listing only the grounded
    // channels also makes a step that grounds two -- which would make the
    // mapping ambiguous -- visible instead of buried.
    // The leading letter is what distinguishes the two designs, and checking it
    // is not pedantry: with the beacon loaded, GP16 decodes "J07" from the ISP
    // header, which has a name and a frame count and looks exactly like a sweep
    // state. Without this the tool drops into sweep mode under the beacon, and
    // reports pull-probe noise off sixteen actively transmitting channels as if
    // it were a grounding result.
    if step.name_hits > 0 && step.name[0] == b'S' {
        l.clear();
        let n = core::str::from_utf8(&step.name).unwrap_or("???");
        let _ = write!(l, "{}  gnd:", n);
        let mut any = false;
        for (i, slot) in LEFT_ROW.iter().enumerate() {
            let Some(ch) = *slot else { continue };
            let (pu, pd) = pull_probe(ch, delay);
            if pu == 0 && pd == 0 {
                any = true;
                let _ = write!(l, " pin{:02}", i + 1);
            }
        }
        if !any {
            let _ = write!(l, " none");
        }
        out.line(&l);
        return;
    }

    // Raw hex dump of GP0, for binary frames the name decoder cannot read.
    // Emitted whenever GP0 carries bytes that are not a beacon name, so the
    // same firmware serves both jobs without a mode switch to forget.
    let mut raw = [0u8; 400];
    let got = decode_raw(buf, 0, &mut raw);
    let ch0 = decode(buf, 0);
    if got > 0 && ch0.name_hits == 0 {
        l.clear();
        let _ = write!(l, "RAW GP0 {} bytes:", got);
        out.line(&l);
        for row in raw[..got].chunks(24) {
            l.clear();
            for b in row {
                let _ = write!(l, "{:02x} ", b);
            }
            out.line(&l);
        }
    }

    l.clear();
    let _ = write!(l, "no sweep state on GP{} (beacon mode, or no jumper)", STEP_CH);
    out.line(&l);

    l.clear();
    let _ = write!(l, "picopin  gpio  name   pull   frames  bytes   err  duty%  minrun");
    out.line(&l);

    let mut identified = 0u32;
    for (i, slot) in LEFT_ROW.iter().enumerate() {
        let pin = i + 1;
        l.clear();
        let Some(ch) = *slot else {
            let _ = write!(l, "   {pin:02}    GND   ----   shorted to ground, not mappable");
            out.line(&l);
            continue;
        };
        let c = decode(buf, ch);
        let (pu, pd) = pull_probe(ch, delay);
        let pull = match (pu, pd) {
            (1, 0) => "isol",   // follows its own pull: nothing driving
            (0, 0) => "GND!",   // held low against a pull-up: board grounds it
            (1, 1) => "HIGH",   // held high against a pull-down
            _ => "????",
        };
        if c.name_hits > 0 {
            identified += 1;
            let name = core::str::from_utf8(&c.name).unwrap_or("???");
            let _ = write!(
                l,
                "   {:02}    GP{:02}  {}   {}   {:04}   {:04}  {:04}   {:03}   {:04}",
                pin, ch, name, pull, c.name_hits, c.bytes_ok, c.framing_err, c.duty_pct, c.min_run
            );
        } else {
            // Say HOW it failed, not just that it did: a line stuck at a rail is
            // a contact problem, traffic that will not frame is a baud problem.
            let why = if c.min_run == 0 && c.duty_pct >= 95 {
                "idle-high (no beacon)"
            } else if c.min_run == 0 {
                "stuck low (no contact? grounded?)"
            } else if c.bytes_ok > 0 {
                "bytes but no name (wrong baud?)"
            } else {
                "activity, no framing"
            };
            let _ = write!(
                l,
                "   {:02}    GP{:02}  ----   {}   {}  (duty {}%, minrun {})",
                pin, ch, pull, why, c.duty_pct, c.min_run
            );
        }
        out.line(&l);
    }

    let mappable = LEFT_ROW.iter().filter(|s| s.is_some()).count();
    l.clear();
    // Every beacon pin shares one sequencer, so a healthy capture has all
    // connected channels within a frame or two of each other.
    let _ = write!(
        l,
        "identified {}/{}; expect ~{} frames/channel at this capture length",
        identified,
        mappable,
        (N_SAMPLES as u32 * BEACON_BAUD) / (SAMPLE_HZ * 50)
    );
    out.line(&l);
}

#[entry]
fn main() -> ! {
    let mut pac = pac::Peripherals::take().unwrap();
    let mut watchdog = hal::Watchdog::new(pac.WATCHDOG);

    let clocks = hal::clocks::init_clocks_and_plls(
        XTAL_HZ,
        pac.XOSC,
        pac.CLOCKS,
        pac.PLL_SYS,
        pac.PLL_USB,
        &mut pac.RESETS,
        &mut watchdog,
    )
    .ok()
    .unwrap();

    let sio = hal::Sio::new(pac.SIO);
    let pins = hal::gpio::Pins::new(
        pac.IO_BANK0,
        pac.PADS_BANK0,
        sio.gpio_bank0,
        &mut pac.RESETS,
    );

    // Every channel becomes a pulled-down input. Pull-down rather than pull-up
    // on purpose: a UART idles high, so an unconnected pulled-up pin is
    // indistinguishable from a healthy idle line, while a pulled-down one reads
    // unambiguously wrong. With the 10k series resistor the divider against the
    // ~50k internal pull still leaves a driven high at ~2.9 V, well above VIH.
    let _ = pins.gpio0.into_pull_down_input();
    let _ = pins.gpio1.into_pull_down_input();
    let _ = pins.gpio2.into_pull_down_input();
    let _ = pins.gpio3.into_pull_down_input();
    let _ = pins.gpio4.into_pull_down_input();
    let _ = pins.gpio5.into_pull_down_input();
    let _ = pins.gpio6.into_pull_down_input();
    let _ = pins.gpio7.into_pull_down_input();
    let _ = pins.gpio8.into_pull_down_input();
    let _ = pins.gpio9.into_pull_down_input();
    let _ = pins.gpio10.into_pull_down_input();
    let _ = pins.gpio11.into_pull_down_input();
    let _ = pins.gpio12.into_pull_down_input();
    let _ = pins.gpio13.into_pull_down_input();
    let _ = pins.gpio14.into_pull_down_input();
    let _ = pins.gpio15.into_pull_down_input();
    let _ = pins.gpio16.into_pull_down_input();
    let _ = pins.gpio17.into_pull_down_input();
    let _ = pins.gpio18.into_pull_down_input();
    let _ = pins.gpio19.into_pull_down_input();
    let _ = pins.gpio20.into_pull_down_input();
    let _ = pins.gpio21.into_pull_down_input();
    let _ = pins.gpio22.into_pull_down_input();
    let _ = pins.gpio26.into_pull_down_input();
    let _ = pins.gpio27.into_pull_down_input();
    let _ = pins.gpio28.into_pull_down_input();

    // One PIO instruction is the whole sampler: read all 32 GPIOs, autopush.
    // PIO input is taken from the pad regardless of function select, so the
    // pins staying SIO inputs above costs nothing here.
    let program = pio::pio_asm!(".wrap_target", "in pins, 32", ".wrap",).program;

    let (mut pio, sm0, _, _, _) = pac.PIO0.split(&mut pac.RESETS);
    let installed = pio.install(&program).unwrap();
    let (sm, rx, _tx) = hal::pio::PIOBuilder::from_installed_program(installed)
        .in_pin_base(0)
        .in_shift_direction(ShiftDirection::Left)
        .autopush(true)
        .push_threshold(32)
        .buffers(Buffers::OnlyRx)
        .clock_divisor_fixed_point(CLKDIV_INT, CLKDIV_FRAC)
        .build(sm0);
    let _sm = sm.start();

    let dma = pac.DMA.split(&mut pac.RESETS);
    let mut ch0 = dma.ch0;
    let mut rx = rx;
    let mut buf: &'static mut [u32; N_SAMPLES] = BUF.init([0u32; N_SAMPLES]);

    let mut delay = cortex_m::delay::Delay::new(
        cortex_m::Peripherals::take().unwrap().SYST,
        clocks.system_clock.freq().to_Hz(),
    );

    let usb_bus = USB_BUS.init(UsbBusAllocator::new(hal::usb::UsbBus::new(
        pac.USBCTRL_REGS,
        pac.USBCTRL_DPRAM,
        clocks.usb_clock,
        true,
        &mut pac.RESETS,
    )));
    let mut serial = SerialPort::new(usb_bus);
    let mut usb_dev = UsbDeviceBuilder::new(usb_bus, UsbVidPid(0x16c0, 0x27dd))
        .strings(&[StringDescriptors::default()
            .manufacturer("xgecu-pro")
            .product("zifmap")
            .serial_number("ZIFMAP1")])
        .unwrap()
        .device_class(usbd_serial::USB_CLASS_CDC)
        .build();
    let mut out = Out {
        serial: &mut serial,
        usb: &mut usb_dev,
    };

    // Let the host enumerate and a terminal attach before anything is printed,
    // otherwise the first report is written into a port nobody is reading yet.
    out.idle(&mut delay, 2500);

    let mut l = Line::new();
    let _ = write!(
        l,
        "zifmap up. sysclk {} Hz, left row = {} mappable contacts + {} grounds",
        clocks.system_clock.freq().to_Hz(),
        LEFT_ROW.iter().filter(|s| s.is_some()).count(),
        LEFT_ROW.iter().filter(|s| s.is_none()).count()
    );
    out.line(&l);

    pad_selftest(&mut delay, &mut out);

    if !selftest(buf, &mut out) {
        l.clear();
        let _ = write!(l, "decoder is broken -- ignore every map below this line");
        out.line(&l);
    }

    let mut pass = 0u32;
    loop {
        pass += 1;
        let xfer = single_buffer::Config::new(ch0, rx, buf).start();
        let (c, r, b) = xfer.wait();
        ch0 = c;
        rx = r;
        buf = b;

        report(buf, pass, &mut out, &mut delay);

        // Re-run continuously so wires can be moved and the map re-read without
        // reflashing. Nothing is driven into the socket; every channel is a
        // pulled-down input.
        out.idle(&mut delay, 1200);
    }
}
