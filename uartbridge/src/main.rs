//! uartbridge — the RP2040 as a plain USB-serial bridge for the T76's ISP header.
//!
//! zifmap captures a window and then decodes it. That is right for one 235-byte
//! frame and structurally wrong for a stream: a PAL/GAL result set is 1 KB, or
//! 5.6 ms at 1.84 Mbaud, and capturing that at 8 samples per bit needs 328 KB
//! against the RP2040's 264 KB. No sample rate rescues it — the approach has a
//! length ceiling and telemetry has no length.
//!
//! So: PIO receives the UART, a software ring absorbs bursts, USB forwards.
//! No window, no ceiling, and it stays useful for anything that streams.
//!
//! The baud rate is selectable at runtime over the CDC port — send '0'..'5'.
//! That is not a convenience: every reflash costs a BOOTSEL cycle and a physical
//! replug, and this rig has spent more time on those than on measurements.

#![no_std]
#![no_main]

use core::fmt::Write as _;
use cortex_m_rt::entry;
use defmt_rtt as _;
use hal::pac;
use hal::pio::{Buffers, PIOExt, ShiftDirection};
use hal::Clock;
use panic_probe as _;
use rp2040_hal as hal;
use static_cell::StaticCell;
use usb_device::bus::UsbBusAllocator;
use usb_device::device::{StringDescriptors, UsbDeviceBuilder, UsbDeviceState, UsbVidPid};
use usbd_serial::SerialPort;

#[link_section = ".boot2"]
#[used]
pub static BOOT2: [u8; 256] = rp2040_boot2::BOOT_LOADER_W25Q080;

const XTAL_HZ: u32 = 12_000_000;

/// The receive pin. GP0 is where every ISP-header jumper on this bench has
/// landed, so keeping it there avoids re-learning a wiring convention.
const RX_PIN: u8 = 0;

/// The transmit pin, GP1 -> ISP header pin 9.
///
/// This is what stops every experiment costing a TD build. With a receiver on
/// the FPGA side the host sends parameters over the wire -- pin directions,
/// sweep bounds, settle time, start/stop -- instead of rebuilding a bitstream
/// and replugging for each one. That loop has been the dominant cost of this
/// work, not the measurements.
const TX_PIN: u8 = 1;

/// The rates the sweep bitstream steps through, in the same order, so a digit
/// sent here names the same rate the FPGA calls that step.
const RATES: [u32; 6] = [114_943, 229_885, 465_116, 909_091, 1_818_182, 4_000_000];

/// PIO runs the receiver at 8 cycles per bit, so the divider is clk/(8*baud).
const CYCLES_PER_BIT: u32 = 8;

/// Ring between the PIO FIFO and USB. Sized to absorb a USB stall: at
/// 1.84 Mbaud that is 183 kB/s, so 8 KB is roughly 45 ms of slack — far longer
/// than a host takes to service a bulk endpoint.
const RING: usize = 8192;

static BUS: StaticCell<UsbBusAllocator<hal::usb::UsbBus>> = StaticCell::new();

struct Ring {
    buf: [u8; RING],
    head: usize,
    tail: usize,
    dropped: u32,
}

impl Ring {
    const fn new() -> Self {
        Ring { buf: [0; RING], head: 0, tail: 0, dropped: 0 }
    }
    #[inline(always)]
    fn push(&mut self, b: u8) {
        let n = (self.head + 1) % RING;
        if n == self.tail {
            // Count the loss rather than overwrite silently. A bridge that
            // drops bytes without saying so turns a transport fault into a
            // data fault, and the receiver has no way to tell them apart.
            self.dropped = self.dropped.saturating_add(1);
            return;
        }
        self.buf[self.head] = b;
        self.head = n;
    }
    #[inline(always)]
    fn len(&self) -> usize {
        (self.head + RING - self.tail) % RING
    }
    fn take(&mut self, out: &mut [u8]) -> usize {
        let mut n = 0;
        while n < out.len() && self.tail != self.head {
            out[n] = self.buf[self.tail];
            self.tail = (self.tail + 1) % RING;
            n += 1;
        }
        n
    }
}

static RINGC: StaticCell<Ring> = StaticCell::new();

#[entry]
fn main() -> ! {
    let mut pac = pac::Peripherals::take().unwrap();
    let mut watchdog = hal::Watchdog::new(pac.WATCHDOG);
    let clocks = hal::clocks::init_clocks_and_plls(
        XTAL_HZ, pac.XOSC, pac.CLOCKS, pac.PLL_SYS, pac.PLL_USB,
        &mut pac.RESETS, &mut watchdog,
    ).ok().unwrap();
    let sys_hz = clocks.system_clock.freq().to_Hz();

    let sio = hal::Sio::new(pac.SIO);
    let pins = hal::gpio::Pins::new(pac.IO_BANK0, pac.PADS_BANK0, sio.gpio_bank0, &mut pac.RESETS);
    // Pull-up, not pull-down: an idle UART line sits HIGH, so a disconnected
    // input then reads as idle rather than as a permanent start bit, which
    // would otherwise fill the ring with framing garbage.
    let _rx = pins.gpio0.into_pull_up_input();
    // The TX pin is driven by PIO, so it is claimed as a PIO function rather
    // than left to SIO.
    let _tx: hal::gpio::Pin<_, hal::gpio::FunctionPio0, hal::gpio::PullNone> =
        pins.gpio1.reconfigure();

    // 8n1 receiver: wait for the start bit, delay to the middle of bit 0, then
    // sample eight bits eight cycles apart.
    let program = pio::pio_asm!(
        ".wrap_target",
        "start:",
        "    wait 0 pin 0",
        "    set x, 7 [10]",
        "bitloop:",
        "    in pins, 1",
        "    jmp x-- bitloop [6]",
        ".wrap",
    ).program;

    // 8n1 transmitter: pull a byte, frame it with a start and stop bit, and
    // clock it out at the same 8 cycles per bit the receiver uses.
    let tx_program = pio::pio_asm!(
        ".side_set 1 opt",
        ".wrap_target",
        "    pull       side 1 [7]",
        "    set x, 7   side 0 [7]",
        "txbit:",
        "    out pins, 1",
        "    jmp x-- txbit [6]",
        ".wrap",
    ).program;

    let (mut pio, sm0, sm1, _, _) = pac.PIO0.split(&mut pac.RESETS);
    let installed = pio.install(&program).unwrap();
    let tx_installed = pio.install(&tx_program).unwrap();
    let (mut sm, mut rx, _tx) = hal::pio::PIOBuilder::from_installed_program(installed)
        .in_pin_base(RX_PIN)
        .in_shift_direction(ShiftDirection::Right)
        .autopush(true)
        .push_threshold(8)
        .buffers(Buffers::OnlyRx)
        .clock_divisor_fixed_point(div_int(sys_hz, RATES[0]), div_frac(sys_hz, RATES[0]))
        .build(sm0);
    sm.set_pindirs([(RX_PIN, hal::pio::PinDir::Input)]);
    let mut sm = sm.start();

    let (mut tsm, _trx, mut ttx) = hal::pio::PIOBuilder::from_installed_program(tx_installed)
        .out_pins(TX_PIN, 1)
        .side_set_pin_base(TX_PIN)
        .out_shift_direction(ShiftDirection::Right)
        .autopull(false)
        .buffers(Buffers::OnlyTx)
        .clock_divisor_fixed_point(div_int(sys_hz, RATES[0]), div_frac(sys_hz, RATES[0]))
        .build(sm1);
    tsm.set_pindirs([(TX_PIN, hal::pio::PinDir::Output)]);
    let mut tsm = tsm.start();

    let usb_bus = BUS.init(UsbBusAllocator::new(hal::usb::UsbBus::new(
        pac.USBCTRL_REGS, pac.USBCTRL_DPRAM, clocks.usb_clock, true, &mut pac.RESETS,
    )));
    let mut serial = SerialPort::new(usb_bus);
    let mut dev = UsbDeviceBuilder::new(usb_bus, UsbVidPid(0x16c0, 0x27dd))
        .strings(&[StringDescriptors::default()
            .manufacturer("xgecu-pro")
            .product("uartbridge")
            .serial_number("UARTBR1")])
        .unwrap()
        .device_class(usbd_serial::USB_CLASS_CDC)
        .build();

    let ring = RINGC.init(Ring::new());
    let mut rate_ix = 0usize;
    let mut out = [0u8; 256];
    let mut cmd = [0u8; 8];
    let mut banner = heapless::String::<96>::new();
    let mut announced = false;

    loop {
        // Drain the PIO FIFO first and hard. Everything else can wait; the FIFO
        // is four entries deep and at 1.84 Mbaud it fills in 22 us.
        while let Some(w) = rx.read() {
            ring.push((w >> 24) as u8);
        }

        dev.poll(&mut [&mut serial]);

        if dev.state() == UsbDeviceState::Configured {
            if !announced {
                announced = true;
                banner.clear();
                let _ = write!(banner, "uartbridge rx GP{} tx GP{} at {} baud; 0-5 changes rate, other bytes go to the FPGA\r\n",
                               RX_PIN, TX_PIN, RATES[rate_ix]);
                let _ = serial.write(banner.as_bytes());
            }
            // A digit selects a rate. Reconfiguring the divider needs the state
            // machine stopped, and the ring is flushed with it: bytes captured
            // at the old rate would decode as noise at the new one.
            if let Ok(n) = serial.read(&mut cmd) {
                for &c in &cmd[..n] {
                    // A bare digit is a local rate change; everything else goes
                    // down the wire to the FPGA. Keeping the escape to a single
                    // character class means the command protocol upstream never
                    // has to know this bridge exists.
                    if !(b'0'..=b'5').contains(&c) {
                        while !ttx.write(u32::from(c)) {
                            dev.poll(&mut [&mut serial]);
                        }
                        continue;
                    }
                    if (b'0'..=b'5').contains(&c) {
                        rate_ix = (c - b'0') as usize;
                        let baud = RATES[rate_ix];
                        sm = {
                            let mut s = sm.stop();
                            s.clock_divisor_fixed_point(div_int(sys_hz, baud), div_frac(sys_hz, baud));
                            s.start()
                        };
                        tsm = {
                            let mut t = tsm.stop();
                            t.clock_divisor_fixed_point(div_int(sys_hz, baud), div_frac(sys_hz, baud));
                            t.start()
                        };
                        while rx.read().is_some() {}
                        ring.head = 0;
                        ring.tail = 0;
                        banner.clear();
                        let _ = write!(banner, "\r\n[rate {} = {} baud, dropped {}]\r\n",
                                       rate_ix, baud, ring.dropped);
                        let _ = serial.write(banner.as_bytes());
                    }
                }
            }
            if ring.len() > 0 {
                let n = ring.take(&mut out);
                let mut sent = 0;
                while sent < n {
                    match serial.write(&out[sent..n]) {
                        Ok(k) if k > 0 => sent += k,
                        _ => {
                            // Keep draining the FIFO while the host is slow.
                            // Blocking here is what overruns the PIO.
                            while let Some(w) = rx.read() {
                                ring.push((w >> 24) as u8);
                            }
                            dev.poll(&mut [&mut serial]);
                        }
                    }
                }
            }
        }
    }
}

/// PIO divider integer part for `baud`, at 8 cycles per bit.
fn div_int(sys_hz: u32, baud: u32) -> u16 {
    ((sys_hz as u64) / (baud as u64 * CYCLES_PER_BIT as u64)) as u16
}
/// ...and its fractional part, in 1/256ths.
fn div_frac(sys_hz: u32, baud: u32) -> u8 {
    let d = (sys_hz as u64 * 256) / (baud as u64 * CYCLES_PER_BIT as u64);
    (d % 256) as u8
}
