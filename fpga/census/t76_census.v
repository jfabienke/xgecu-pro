// ---------------------------------------------------------------------------
// T76 static census -- stage 1 of the FPGA-side interface inventory.
//
// Samples every board net the FPGA can reach and reports, for each one:
//   * its instantaneous level at the moment the frame was latched
//   * whether it was ever low  during the preceding window
//   * whether it was ever high during the preceding window
//
// Those three bits per pin separate "tied low", "tied high" and "active"
// without decoding any protocol.  That is enough to read the FPGA's
// configuration straps, settle whether the JTAG pins really are unconnected,
// classify the unlabelled ball at C4, and -- the point of the exercise -- show
// which of the 24 wired HD lines actually move.
//
// Deliberate design choices:
//
//   * Every observed pin is declared `input`, never `inout`.  The toolchain
//     therefore cannot infer a driver, so the FPGA physically cannot contend
//     with the MCU.  This is structural, not a matter of being careful.
//
//   * The sampling clock is CLK_20 (ball E10), which is independent of the bus
//     under test.  Clocking from HTCLK would be more elegant but would make a
//     dead bus indistinguishable from a broken bitstream: with CLK_20 we still
//     get frames out, reporting the bus as silent.
//
//   * Rail control is driven to a static state rather than left floating, so
//     the ZIF rail drivers cannot do something undefined.  Their polarity is
//     undocumented, which is why the socket MUST be empty.  See README.md.
//
// Output is a repeating binary frame on the ISP header, decoded by
// decode_census.py.  Pin ordering comes from pinmap.json; all three files are
// generated from the same source by gen_census.py and cannot drift apart.
// ---------------------------------------------------------------------------

`default_nettype none

module t76_census #(
    // Overridden by the testbench so simulation need not run 250 ms.
    parameter integer FRAME_GAP = 5_000_000
) (
`include "census_ports.vh"
);

`include "census_params.vh"
`include "census_obs.vh"
`include "census_hd_bus.vh"

localparam integer CLK_HZ    = 20_000_000;
localparam integer BAUD      = 115200;
localparam integer BAUD_DIV  = CLK_HZ / BAUD;          // 173.6 -> 174, 0.2% error
localparam integer CAPN      = 63;
localparam integer SKIP      = 126;  // window start, in words (packets 18+)   // 9 whole packets of 7 words                     // captured HSPI words
localparam integer HDR       = 8;                      // preamble+version+npins+ndetail+capn
localparam integer EDGE_OFF  = HDR + 3*NBYTES;         // transition counters
localparam integer STAT_OFF  = EDGE_OFF + 2*NDETAIL;   // capwords, bursts
localparam integer CAP_OFF   = STAT_OFF + 4;           // captured words, 4 bytes each
localparam integer FRAME_LEN = CAP_OFF + 2*CAPN + 2;

// --- rail control: static, safe, never floating ----------------------------
// Rail control. The three output enables are held at 1, and that value is
// MEASURED, not assumed: the rail sweep (fpga/railsweep) walked all four
// combinations of register content and gnd_oe while an RP2040 in the socket
// read levels, and only "register all-ones AND gnd_oe = 0" grounded the socket.
// So a register bit of 1 selects a position, and the enables are ACTIVE LOW --
// 0 drives, 1 releases. Standard for 595-class parts, where the enable is OE.
//
// Every earlier revision of this file held them at 0, believing that was "off".
// It is the ENABLED state. The ground rail was therefore driving the socket for
// the whole of the census and beacon work, with whatever pattern happened to be
// left in the shift register -- which is why 42 socket positions looked
// "isolated" when they were being grounded by this design, and why the beacon
// was driving 48 outputs into a ground switch the entire time.
//
// The latch enables stay at 0: that holds the latches closed on their current
// contents rather than making them transparent.
assign ser_clk = 1'b0;
assign ser_data = 1'b0;
assign vpp_le  = 1'b0;
assign vcc_le  = 1'b0;
assign gnd_le  = 1'b0;
assign vpp_oe  = 1'b1;   // released (active low)
assign vcc_oe  = 1'b1;   // released (active low)
assign gnd_oe  = 1'b1;   // released (active low)

`include "census_isp_power.vh"

// No reset network. Every register carries a declared initial value, which the
// bitstream loads at configuration -- the idiomatic approach on an SRAM FPGA,
// and the one radiomanV's working T76 design uses.
//
// This is not merely a simplification. An outer `if (rst)` makes TD infer a
// dedicated set/reset pin on each flop, and it then refuses to pack any flop
// that would need both (a constant-1 load is a set, the reset is a reset),
// aborting with SYN-8700 in pack::SeqSetReset. Without the wrapper the same
// logic becomes ordinary data-path muxing, which packs cleanly.

// --- two-flop synchroniser on every observed pin ---------------------------
reg [NPINS-1:0] sync_a = {NPINS{1'b0}};
reg [NPINS-1:0] sync_b = {NPINS{1'b0}};
always @(posedge i_clock_20M) begin
    sync_a <= obs;
    sync_b <= sync_a;
end

// --- sticky activity across the reporting window ---------------------------
reg [NPINS-1:0] ever_hi = {NPINS{1'b0}};
reg [NPINS-1:0] ever_lo = {NPINS{1'b0}};

// --- transition counting on the MCU link -----------------------------------
// Edge count is what answers the question this instrument exists for. Uniform
// plaintext barely toggles a data line; AES or SM4 ciphertext toggles it about
// half the time regardless of what was fed in. So the *rate* separates them,
// with no need to decode a single byte or measure a duty cycle.
//
// Confined to indices 0..NDETAIL-1 (the HSPI/BUS candidates): the ZIF and ISP
// pins are static and counting them would cost 3x the logic for nothing.
reg [NPINS-1:0]  sync_prev = {NPINS{1'b0}};
reg [15:0]       edges  [0:NDETAIL-1];
reg [15:0]       f_edge [0:NDETAIL-1];
integer ei;
initial for (ei = 0; ei < NDETAIL; ei = ei + 1) begin
    edges[ei]  = 16'd0;
    f_edge[ei] = 16'd0;
end

// --- HTRDY: the receiver-ready handshake -----------------------------------
// CH569 Table 10-1: HTRDY is a pull-down INPUT on the MCU ("Detect the status of
// reception end"), so the receiving end drives it. Section 10.2.3: the MCU
// raises HTREQ, "if transmission is allowed at the lower end, its hardware will
// drive HTRDY to high level output", and only then does it assert HTVLD and
// clock the packet out.
//
// Without this the MCU raises HTREQ and waits forever -- measured: HTREQ took
// exactly one transition and every HD line stayed static.
//
// HTACK is deliberately NOT driven. It is the MCU's own push-pull OUTPUT; the
// FPGA driving it would be direct contention.
reg htreq_s0 = 1'b0, htreq_s1 = 1'b0;
always @(posedge i_clock_20M) begin
    htreq_s0 <= HTREQ;
    htreq_s1 <= htreq_s0;
end
// Synchronised echo of HTREQ, deliberately, despite this making `erase` report
// failure. Measured trade-off, same chip and same windows:
//
//   synchronised HTRDY : erase exits 1, but every captured packet's CRC is VALID
//   static HTRDY       : erase exits 0, but the CRC word of every packet reads 0
//
// Headers and payloads are byte-identical either way -- only the final word of
// each packet is lost when HTRDY is held high, because the MCU's timing shifts.
// For protocol mapping the verified packet is worth more than the exit code, so
// the slower handshake stays. Revert to `assign htrdy = 1'b1;` when a completing
// operation matters more than a checkable capture.
assign htrdy = htreq_s1;

// --- capture the packet in the MCU's own clock domain -----------------------
// HD[] is only meaningful on HTCLK edges while HTVLD is high, and HTCLK runs far
// faster than our 20 MHz sampling clock -- sampling it here would alias, exactly
// as the HTCLK transition counter does. So the capture runs on HTCLK itself,
// which is what a real HSPI receiver does.
reg [15:0] capbuf [0:CAPN-1];
reg [7:0]  capcnt   = 8'd0;
reg [7:0]  skipped  = 8'd0;    // words captured in the current burst
reg [15:0] capwords = 16'd0;   // total valid words seen, saturating
reg [15:0] bursts   = 16'd0;   // HTVLD rising edges, saturating
reg        htvld_d  = 1'b0;
integer ci;
initial for (ci = 0; ci < CAPN; ci = ci + 1) capbuf[ci] = 16'd0;

// Registers in the HTCLK domain do NOT take their declared initial values:
// HTCLK free-runs during FPGA configuration, so they clock before
// initialisation settles. Measured twice -- capfull came up set, which stopped
// the capture writing at all, and skipped came up >= SKIP, which bypassed the
// window offset. Both looked like logic bugs and were neither.
//
// The 20 MHz domain does initialise reliably, so it arms this one. The two-flop
// synchroniser converges on arm's real value within two HTCLK edges whatever
// those flops powered up as, so the clear always happens.
reg [15:0] arm_cnt = 16'd0;
reg        arm     = 1'b0;
always @(posedge i_clock_20M)
    if (!arm) begin
        arm_cnt <= arm_cnt + 16'd1;
        if (arm_cnt == 16'hFFFF) arm <= 1'b1;
    end

reg arm_s0 = 1'b0, arm_s1 = 1'b0;

// Sampling on the FALLING edge of HTCLK. 10.2.3 notes the receiving end's
// sampling edge is configurable (HSPI_AUX), so there are two valid choices and
// the first was picked blindly. With HTRDY held statically high the MCU starts
// its data phase at a different point relative to HTVLD, and rising-edge
// sampling then landed on undriven cycles: headers came through intact while
// every payload and CRC word read as zero.
always @(posedge HTCLK) begin
    arm_s0 <= arm;
    arm_s1 <= arm_s0;
    htvld_d <= HTVLD;
    // capcnt is deliberately NOT rewound at each burst. Rewinding meant the
    // buffer always held the *last* burst, and which packet that turned out to
    // be depended on where the run stopped -- so cross-run comparisons compared
    // non-corresponding packets, and a free-running sequence number read as
    // "chip parameters" and "an opcode". Filling once and stopping gives a
    // deterministic position: the first words after configuration, every time.
    //
    // One capture per upload, therefore. Uploads are cheap and erase does not
    // wedge, so a data point costs an upload plus two erases.
    if (!arm_s1) begin
        skipped  <= 8'd0;
        capcnt   <= 8'd0;
        capwords <= 16'd0;
        bursts   <= 16'd0;
    end else begin
    if (HTVLD && !htvld_d) begin
        if (bursts != 16'hFFFF) bursts <= bursts + 16'd1;
    end
    if (HTVLD) begin
        // Skip SKIP words before capturing, so the window can be moved along the
        // sequence without widening the frame. 63 words is 9 packets, and the
        // frame stays at 241 bytes -- under the 255 an 8-bit byte index can
        // address. SKIP = 0 gives packets 0-8, 63 gives 9-17, 126 gives 18-20.
        if (skipped < SKIP[7:0])
            skipped <= skipped + 8'd1;
        else if (capcnt < CAPN[7:0]) begin
            capbuf[capcnt] <= hd_bus[15:0];
            capcnt <= capcnt + 8'd1;
        end
        if (capwords != 16'hFFFF) capwords <= capwords + 16'd1;
    end
    end
end

// Frame-side copies. Refreshed only while HTVLD is low, so a burst in flight is
// never latched half-written -- the clock-domain crossing is resolved by only
// reading data that has stopped changing.
reg [15:0] f_cap [0:CAPN-1];
reg [15:0] f_capwords = 16'd0;
reg [15:0] f_bursts   = 16'd0;
reg        htvld_s0 = 1'b0, htvld_s1 = 1'b0;
always @(posedge i_clock_20M) begin
    htvld_s0 <= HTVLD;
    htvld_s1 <= htvld_s0;
end
initial for (ci = 0; ci < CAPN; ci = ci + 1) f_cap[ci] = 16'd0;

// --- frame timer -----------------------------------------------------------
reg [22:0] gap = 23'd0;
wire       frame_due = (gap == FRAME_GAP[22:0]);

// --- latched frame payload, zero-padded to a byte boundary -----------------
reg [NBYTES*8-1:0] f_snap = {(NBYTES*8){1'b0}};
reg [NBYTES*8-1:0] f_lo   = {(NBYTES*8){1'b0}};
reg [NBYTES*8-1:0] f_hi   = {(NBYTES*8){1'b0}};

// --- UART transmitter ------------------------------------------------------
// The framing bits are produced by an output mux instead of being shifted
// through the register. Loading a constant 1 into a flop (the UART stop bit)
// makes TD infer a set alongside the reset, which the Eagle fabric does not
// support -- it rejects the design with SYN-8700. Holding only real data in
// registers avoids the whole class of problem.
reg [7:0]  tx_byte = 8'd0;
reg [3:0]  tx_bit  = 4'd0;   // 0 = start, 1..8 = data LSB first, 9 = stop
reg [8:0]  tx_div  = 9'd0;
reg        tx_busy = 1'b0;
reg [7:0]  tx_data = 8'd0;
reg        tx_load = 1'b0;

assign uart_tx = (!tx_busy)       ? 1'b1 :                       // idle
                 (tx_bit == 4'd0) ? 1'b0 :                       // start
                 (tx_bit <= 4'd8) ? tx_byte[tx_bit - 4'd1] :     // data
                                    1'b1;                        // stop

always @(posedge i_clock_20M) begin
    if (tx_load && !tx_busy) begin
        tx_byte <= tx_data;
        tx_bit  <= 4'd0;
        tx_div  <= BAUD_DIV[8:0] - 9'd1;
        tx_busy <= 1'b1;
    end else if (tx_busy) begin
        if (tx_div != 9'd0) begin
            tx_div <= tx_div - 9'd1;
        end else begin
            tx_div <= BAUD_DIV[8:0] - 9'd1;
            if (tx_bit == 4'd9) tx_busy <= 1'b0;
            else                tx_bit  <= tx_bit + 4'd1;
        end
    end
end

// --- CRC-16/CCITT-FALSE over bytes 4 .. FRAME_LEN-3 ------------------------
function [15:0] crc16_step;
    input [15:0] c;
    input [7:0]  d;
    integer i;
    reg [15:0] x;
    begin
        x = c ^ {d, 8'h00};
        for (i = 0; i < 8; i = i + 1)
            x = x[15] ? ((x << 1) ^ 16'h1021) : (x << 1);
        crc16_step = x;
    end
endfunction

reg [15:0] crc = 16'd0;   // seeded to 16'hFFFF at each frame start

// --- frame byte selection --------------------------------------------------
reg [7:0] byte_idx = 8'd0;

function [7:0] frame_byte;
    input [7:0] i;
    begin
        if      (i == 8'd0 || i == 8'd2) frame_byte = 8'h55;
        else if (i == 8'd1 || i == 8'd3) frame_byte = 8'hAA;
        else if (i == 8'd4)              frame_byte = 8'h03;               // version
        else if (i == 8'd5)              frame_byte = NPINS[7:0];
        else if (i == 8'd6)              frame_byte = NDETAIL[7:0];
        else if (i == 8'd7)              frame_byte = CAPN[7:0];
        else if (i <  HDR +   NBYTES)    frame_byte = f_snap[(i-HDR)*8            +: 8];
        else if (i <  HDR + 2*NBYTES)    frame_byte = f_lo  [(i-HDR-NBYTES)*8     +: 8];
        else if (i <  HDR + 3*NBYTES)    frame_byte = f_hi  [(i-HDR-2*NBYTES)*8   +: 8];
        else if (i <  EDGE_OFF + 2*NDETAIL)
                                         frame_byte = (i[0] == (EDGE_OFF[0]))
                                             ? f_edge[(i-EDGE_OFF)>>1][7:0]
                                             : f_edge[(i-EDGE_OFF)>>1][15:8];
        else if (i == STAT_OFF)             frame_byte = f_capwords[7:0];
        else if (i == STAT_OFF + 1)         frame_byte = f_capwords[15:8];
        else if (i == STAT_OFF + 2)         frame_byte = f_bursts[7:0];
        else if (i == STAT_OFF + 3)         frame_byte = f_bursts[15:8];
        else if (i <  CAP_OFF + 2*CAPN)
                                            // Byte lane must come from the offset
                                            // WITHIN the capture region: CAP_OFF
                                            // is not a multiple of 4, so i[1:0]
                                            // skews every word by CAP_OFF mod 4.
                                            frame_byte = f_cap[(i-CAP_OFF)>>1][((i-CAP_OFF)&8'd1)*8 +: 8];
        else if (i == CAP_OFF + 2*CAPN)     frame_byte = crc[15:8];
        else                                frame_byte = crc[7:0];
    end
endfunction

// --- main sequencer --------------------------------------------------------
localparam S_ARM = 1'b0, S_SEND = 1'b1;
reg state = S_ARM;

always @(posedge i_clock_20M) begin
    tx_load <= 1'b0;

    begin
        // Accumulate activity continuously; it is only ever cleared at the
        // moment a frame is latched, so each frame describes exactly the
        // window since the previous one.
        ever_hi   <= ever_hi |  sync_b;
        ever_lo   <= ever_lo | ~sync_b;
        sync_prev <= sync_b;
        for (ei = 0; ei < NDETAIL; ei = ei + 1)
            if (state == S_ARM && frame_due) edges[ei] <= 16'd0;
            else if (sync_b[ei] != sync_prev[ei] && edges[ei] != 16'hFFFF)
                edges[ei] <= edges[ei] + 16'd1;

        case (state)
        S_ARM: begin
            if (frame_due) begin
                f_snap   <= {{(NBYTES*8-NPINS){1'b0}}, sync_b};
                f_lo     <= {{(NBYTES*8-NPINS){1'b0}}, ever_lo | ~sync_b};
                f_hi     <= {{(NBYTES*8-NPINS){1'b0}}, ever_hi |  sync_b};
                ever_hi  <= {NPINS{1'b0}};
                ever_lo  <= {NPINS{1'b0}};
                for (ei = 0; ei < NDETAIL; ei = ei + 1) f_edge[ei] <= edges[ei];
                if (!htvld_s1) begin
                    f_capwords <= capwords;
                    f_bursts   <= bursts;
                    for (ci = 0; ci < CAPN; ci = ci + 1) f_cap[ci] <= capbuf[ci];
                end
                gap      <= 23'd0;
                byte_idx <= 8'd0;
                crc      <= 16'hFFFF;
                state    <= S_SEND;
            end else begin
                gap <= gap + 23'd1;
            end
        end

        S_SEND: begin
            if (!tx_busy && !tx_load) begin
                tx_data <= frame_byte(byte_idx);
                tx_load <= 1'b1;
                // CRC covers the version byte through the last payload byte.
                if (byte_idx >= 8'd4 && byte_idx < (CAP_OFF + 2*CAPN))
                    crc <= crc16_step(crc, frame_byte(byte_idx));
                if (byte_idx == FRAME_LEN - 1) state <= S_ARM;
                else byte_idx <= byte_idx + 8'd1;
            end
        end
        endcase
    end
end

endmodule

`default_nettype wire
