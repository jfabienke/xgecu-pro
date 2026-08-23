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
    parameter integer FRAME_GAP = 5_000_000,
    // Shrunk by the testbench so a burst can actually fill the one-shot buffer.
    parameter integer CAPDEPTH  = 256
) (
`include "census_ports.vh"
);

`include "census_params.vh"
`include "census_obs.vh"
`include "census_hd_bus.vh"

localparam integer CLK_HZ    = 20_000_000;
localparam integer BAUD      = 115200;
localparam integer BAUD_DIV  = CLK_HZ / BAUD;          // 173.6 -> 174, 0.2% error
localparam integer CAPN      = 32;                     // words reported per frame
localparam integer NWIN      = CAPDEPTH / CAPN;        // frames to ship it all
localparam integer HDR       = 8;                      // preamble+version+npins+ndetail+capn
localparam integer EDGE_OFF  = HDR + 3*NBYTES;         // transition counters
localparam integer STAT_OFF  = EDGE_OFF + 2*NDETAIL;   // capwords, bursts, win, flags
localparam integer CAP_OFF   = STAT_OFF + 6;           // captured words, 4 bytes each
localparam integer FRAME_LEN = CAP_OFF + 4*CAPN + 2;

// --- rail control: static, safe, never floating ----------------------------
assign ser_clk = 1'b0;
assign ser_data = 1'b0;
assign vpp_le  = 1'b0;
assign vcc_le  = 1'b0;
assign gnd_le  = 1'b0;
assign vpp_oe  = 1'b0;
assign vcc_oe  = 1'b0;
assign gnd_oe  = 1'b0;

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
assign htrdy = htreq_s1;   // ready whenever asked

// --- capture the packet in the MCU's own clock domain -----------------------
// HD[] is only meaningful on HTCLK edges while HTVLD is high, and HTCLK runs far
// faster than our 20 MHz sampling clock -- sampling it here would alias, exactly
// as the HTCLK transition counter does. So the capture runs on HTCLK itself,
// which is what a real HSPI receiver does.
// One-shot and continuous across bursts. Resetting per burst kept only the
// first 32 words of the LAST burst, which is why the bulk payload was never
// seen: 308 words crossed the link and we reported 32 of them. Now the buffer
// fills once, from the first valid word, and then freezes -- so what it holds
// is the opening of the whole exchange rather than a late fragment.
reg [23:0] capbuf [0:CAPDEPTH-1];
reg [8:0]  capcnt   = 9'd0;
reg        capfull  = 1'b0;
reg [15:0] capwords = 16'd0;   // total valid words seen, saturating
reg [15:0] bursts   = 16'd0;   // HTVLD rising edges, saturating
reg        htvld_d  = 1'b0;
integer ci;
initial for (ci = 0; ci < CAPDEPTH; ci = ci + 1) capbuf[ci] = 24'd0;

always @(posedge HTCLK) begin
    htvld_d <= HTVLD;
    if (HTVLD && !htvld_d && bursts != 16'hFFFF) bursts <= bursts + 16'd1;
    if (HTVLD) begin
        if (!capfull) begin
            capbuf[capcnt] <= hd_bus;
            if (capcnt == CAPDEPTH[8:0] - 9'd1) capfull <= 1'b1;
            else capcnt <= capcnt + 9'd1;
        end
        if (capwords != 16'hFFFF) capwords <= capwords + 16'd1;
    end
end

// capbuf is read straight from the 20 MHz side, which is safe *only* once the
// one-shot has frozen: after that nothing writes it. Before then the frame ships
// zeros and clears the frozen flag, so a partial buffer can never be mistaken
// for data. That is the whole clock-domain-crossing argument -- no handshake
// needed, because the data stops changing.
reg [15:0] f_capwords = 16'd0;
reg [15:0] f_bursts   = 16'd0;
reg [7:0]  f_win      = 8'd0;
reg        f_full     = 1'b0;
reg [7:0]  win        = 8'd0;
reg        capfull_s0 = 1'b0, capfull_s1 = 1'b0;
always @(posedge i_clock_20M) begin
    capfull_s0 <= capfull;
    capfull_s1 <= capfull_s0;
end

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

reg [7:0] byte_idx = 8'd0;

// --- registered read port for the capture buffer ----------------------------
// TD infers block RAM for a 256-entry array (which is why the deeper buffer
// costs LESS logic than the 32-entry register version did). Block RAM reads are
// SYNCHRONOUS: the combinational read this used to do returned the array's
// initial contents, so every captured word shipped as zero while the counters
// correctly reported 308 words captured.
//
// That failure is invisible in simulation -- iverilog models the array as
// memory and an asynchronous read works there -- so it can only be caught on
// hardware, or by knowing the rule.
//
// Bytes leave at 115200 baud, ~1740 clocks apart, so a two-cycle address/data
// latency is free.
reg [7:0]  cap_addr = 8'd0;
reg [23:0] cap_rd   = 24'd0;
always @(posedge i_clock_20M) begin
    cap_addr <= {f_win[2:0], 5'd0}
                + ((byte_idx >= CAP_OFF[7:0]) ? ((byte_idx - CAP_OFF[7:0]) >> 2) : 8'd0);
    cap_rd   <= capbuf[cap_addr];
end

// --- frame byte selection --------------------------------------------------

function [7:0] frame_byte;
    input [7:0] i;
    reg [31:0] cw;
    begin
        if      (i == 8'd0 || i == 8'd2) frame_byte = 8'h55;
        else if (i == 8'd1 || i == 8'd3) frame_byte = 8'hAA;
        else if (i == 8'd4)              frame_byte = 8'h04;               // version
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
        else if (i == STAT_OFF + 4)         frame_byte = f_win;
        else if (i == STAT_OFF + 5)         frame_byte = {7'd0, f_full};
        else if (i <  CAP_OFF + 4*CAPN) begin
            // Byte lane must come from the offset WITHIN the capture region:
            // CAP_OFF is not a multiple of 4, so i[1:0] would skew every word
            // by CAP_OFF mod 4 -- a bug tb_hspi caught before hardware did.
            cw = {8'd0, cap_rd};
            frame_byte = cw[((i - CAP_OFF) & 8'd3) * 8 +: 8];
        end
        else if (i == CAP_OFF + 4*CAPN)     frame_byte = crc[15:8];
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
                f_capwords <= capwords;
                f_bursts   <= bursts;
                f_full     <= capfull_s1;
                f_win      <= win;
                win        <= (win == NWIN[7:0] - 8'd1) ? 8'd0 : win + 8'd1;
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
                if (byte_idx >= 8'd4 && byte_idx < (CAP_OFF + 4*CAPN))
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
