// ---------------------------------------------------------------------------
// T76 rail hold: one static state, held indefinitely.
//
// Answering a single question the walking sweep left open. Z22, Z23 and Z24
// never grounded at any step, despite Z23 sitting on a well-contacted pin 02
// and Z24 on pin 01, both confirmed by the beacon at 32 frames minutes before
// each run. Three explanations, and they need separating rather than arguing:
//
//   A. those positions are not in the ground chain at all
//   B. chain bits 16/21/22 drive something other than socket pins
//   C. the sweep missed them -- timing, or contact during those 4 seconds
//
// C is the boring explanation and it is my code, so it goes first. Phase 1
// showed all-ones + enable grounds the socket, but the Pico was at offset
// pin+24 then, covering Z25..Z44. ALL-ONES HAS NEVER BEEN APPLIED TO Z22-Z24.
//
//   grounds them     -> the chain does drive them, the sweep missed them  (C)
//   leaves them high -> they are genuinely not GND-switchable          (A or B)
//
// Static, not swept, for two reasons. It removes every timing question from the
// Pico's side; and it lets a METER read Z23 and Z24 directly, which depends on
// none of the pull probe, the decoder, or contact timing -- all three of which
// have produced confident wrong answers in this work.
//
// SAFETY: GND only. VPP and VCC latches and enables are untouched and released.
// The ZIF pins are not declared, so the FPGA drives nothing into the socket.
// ---------------------------------------------------------------------------

`default_nettype none

module t76_railhold (
`include "railhold_ports.vh"
);

localparam integer BAUD_DIV  = 20_000_000 / 115200;
localparam integer SHIFT_DIV = 64;
localparam integer NBITS     = 48;

// ISP header power/ground. 26 measured at true ground (0 V) and is asserted so
// the header keeps a reference. 27 and 28 are NOT asserted: with the beacon
// asserting them they measured -3 V against the USB-C shell, and there is no
// reason to energize an undocumented negative rail during an experiment that
// does not need it.
assign j_gnd_11 = 1'b1;
assign j_gnd_21 = 1'b1;
assign j_gnd_26 = 1'b1;
assign j_gnd_27 = 1'b0;
assign j_gnd_28 = 1'b0;
assign j_vcc_04 = 1'b0;
assign j_vcc_20 = 1'b0;
assign j_vcc_22 = 1'b0;
assign j_vcc_24 = 1'b0;
assign j_vpp_24 = 1'b0;
assign j_vpp_26 = 1'b0;

// VPP and VCC domains stay untouched for the whole of phase 1.
assign vpp_le = 1'b0;
assign vcc_le = 1'b0;
assign vpp_oe = 1'b0;
assign vcc_oe = 1'b0;

// --- shift all ones in once, latch, then assert and hold ---------------------
localparam [1:0] P_SHIFT = 2'd0, P_LATCH = 2'd1, P_HOLD = 2'd2;

reg [1:0]  phase  = P_SHIFT;
reg [15:0] divcnt = 16'd0;
reg [7:0]  bitcnt = 8'd0;
reg        sclk   = 1'b0;
reg        g_le   = 1'b0;
reg        g_oe   = 1'b1;      // released (active low)

wire tick = (divcnt == SHIFT_DIV[15:0] - 16'd1);

always @(posedge i_clock_20M) begin
    divcnt <= tick ? 16'd0 : divcnt + 16'd1;
    case (phase)
    P_SHIFT: begin
        g_oe <= 1'b1;                     // never drive a half-shifted register
        g_le <= 1'b0;
        if (tick) begin
            sclk <= ~sclk;
            if (sclk) begin
                bitcnt <= bitcnt + 8'd1;
                if (bitcnt == NBITS[7:0] - 8'd1) phase <= P_LATCH;
            end
        end
    end
    P_LATCH: begin
        sclk <= 1'b0;
        g_le <= 1'b1;
        if (tick) begin
            g_le  <= 1'b0;
            phase <= P_HOLD;
        end
    end
    default: begin                        // P_HOLD -- and it stays here
        g_oe <= 1'b0;                     // asserted (active low)
    end
    endcase
end

assign ser_clk  = sclk;
assign ser_data = 1'b1;                   // all ones
assign gnd_le   = g_le;
assign gnd_oe   = g_oe;

// --- UART: announce a fixed "H01\r\n" so the tool knows a hold is running --------------------------
// Same framing and the same real baud rate as the beacon (20e6/174 = 114942),
// so zifmap decodes it with no change to its bit timing.
reg [8:0] ubdiv   = 9'd0;
reg [3:0] ubit    = 4'd0;   // 0 start, 1..8 data LSB first, 9 stop
reg [2:0] ubyte   = 3'd0;   // 0 'S', 1 tens, 2 units, 3 CR, 4 LF

always @(posedge i_clock_20M) begin
    if (ubdiv == 9'd0) begin
        ubdiv <= BAUD_DIV[8:0] - 9'd1;
        if (ubit == 4'd9) begin
            ubit  <= 4'd0;
            ubyte <= (ubyte == 3'd4) ? 3'd0 : ubyte + 3'd1;
        end else begin
            ubit <= ubit + 4'd1;
        end
    end else begin
        ubdiv <= ubdiv - 9'd1;
    end
end

// Passed as arguments, not read from module scope: a continuous assignment
// calling a function re-evaluates only when its ARGUMENTS change, and reading
// the counters implicitly freezes the output at its time-zero value -- which
// simulates as a dead line and on hardware looks exactly like a broken board.
function tx_bit;
    input [7:0] t, u;
    input [2:0] by;
    input [3:0] bi;
    reg   [7:0] b;
    begin
        case (by)
            3'd0:    b = 8'h48;       // 'H'
            3'd1:    b = t;
            3'd2:    b = u;
            3'd3:    b = 8'h0D;       // CR
            default: b = 8'h0A;       // LF
        endcase
        case (bi)
            4'd0:    tx_bit = 1'b0;               // start
            4'd9:    tx_bit = 1'b1;               // stop
            default: tx_bit = b[bi - 4'd1];       // LSB first
        endcase
    end
endfunction

assign uart_tx = tx_bit(8'h30, 8'h31, ubyte, ubit);   // "H01"

endmodule

`default_nettype wire
