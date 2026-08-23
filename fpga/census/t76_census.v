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

localparam integer CLK_HZ    = 20_000_000;
localparam integer BAUD      = 115200;
localparam integer BAUD_DIV  = CLK_HZ / BAUD;          // 173.6 -> 174, 0.2% error
localparam integer HDR       = 6;                      // preamble+version+count
localparam integer FRAME_LEN = HDR + 3*NBYTES + 2;     // + three bitfields + CRC

// --- rail control: static, safe, never floating ----------------------------
assign ser_clk = 1'b0;
assign ser_data = 1'b0;
assign vpp_le  = 1'b0;
assign vcc_le  = 1'b0;
assign gnd_le  = 1'b0;
assign vpp_oe  = 1'b0;
assign vcc_oe  = 1'b0;
assign gnd_oe  = 1'b0;

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
        else if (i == 8'd4)              frame_byte = 8'h01;               // version
        else if (i == 8'd5)              frame_byte = NPINS[7:0];
        else if (i <  HDR +   NBYTES)    frame_byte = f_snap[(i-HDR)*8            +: 8];
        else if (i <  HDR + 2*NBYTES)    frame_byte = f_lo  [(i-HDR-NBYTES)*8     +: 8];
        else if (i <  HDR + 3*NBYTES)    frame_byte = f_hi  [(i-HDR-2*NBYTES)*8   +: 8];
        else if (i == HDR + 3*NBYTES)    frame_byte = crc[15:8];
        else                             frame_byte = crc[7:0];
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
        ever_hi <= ever_hi |  sync_b;
        ever_lo <= ever_lo | ~sync_b;

        case (state)
        S_ARM: begin
            if (frame_due) begin
                f_snap   <= {{(NBYTES*8-NPINS){1'b0}}, sync_b};
                f_lo     <= {{(NBYTES*8-NPINS){1'b0}}, ever_lo | ~sync_b};
                f_hi     <= {{(NBYTES*8-NPINS){1'b0}}, ever_hi |  sync_b};
                ever_hi  <= {NPINS{1'b0}};
                ever_lo  <= {NPINS{1'b0}};
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
                if (byte_idx >= 8'd4 && byte_idx < (HDR + 3*NBYTES))
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
