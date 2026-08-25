// ---------------------------------------------------------------------------
// T76 rail sweep, phase 1: does gnd_oe do anything, and at which polarity?
//
// The ZIF socket's per-pin driver network is controlled by a 595-style chain:
// ser_clk/ser_data shift a pattern in, a per-domain latch enable (vpp_le,
// vcc_le, gnd_le) captures it, and a per-domain output enable (vpp_oe, vcc_oe,
// gnd_oe) drives it onto the socket. Bit order and BOTH polarities are unknown,
// which is why 42 of 48 socket positions measured as isolated: with every
// control held at 0, nothing reaches the socket.
//
// This asks the cheapest question first. Four states, each announced over the
// ISP UART, each held long enough to read:
//
//     S00   register all-zero, gnd_oe = 0     <- the known-safe resting state
//     S01   register all-zero, gnd_oe = 1
//     S02   register all-one,  gnd_oe = 0
//     S03   register all-one,  gnd_oe = 1
//
// If any socket position changes level across those four, we have found the
// enable and its polarity in one run. If nothing moves, the register is longer
// than 48 bits or the latch needs different handling -- also worth knowing, and
// far cheaper to learn here than buried inside a 48-step walking-ones sweep.
//
// GND is deliberately the first domain touched. Grounding a socket pin is the
// least dangerous thing this board can be asked to do: no VCC, no VPP, nothing
// above 3.3 V anywhere in the experiment. VPP is 12 V+ and gets characterised
// only once this register is understood.
//
// SAFETY: the ZIF pins are NOT declared, so the FPGA never drives them. That is
// not tidiness -- the beacon drives all 48, and a ground switch closing on a
// driven pin is an FPGA output fighting a ground switch. The census source
// already warns about exactly this. Whatever is in the socket reads levels; the
// FPGA only drives rail control and the one UART pin.
//
// No reset network: TD refuses to pack a flop needing both set and reset
// (SYN-8700), so state is established by declared initial values, loaded at
// configuration.
// ---------------------------------------------------------------------------

`default_nettype none

module t76_railsweep (
`include "railsweep_ports.vh"
);

localparam integer BAUD_DIV  = 20_000_000 / 115200;   // 173.6 -> 174
localparam integer SHIFT_DIV = 64;                    // ~312 kHz serial clock
localparam integer NBITS     = 48;                    // assumed chain length
parameter  integer HOLD_TICKS = 20_000_000 * 2;        // ~2 s per state (tb overrides)

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

// --- experiment sequencer ---------------------------------------------------
localparam [1:0] P_SHIFT = 2'd0, P_LATCH = 2'd1, P_HOLD = 2'd2;

reg [1:0]  state   = 2'd0;      // which of the four experiments
reg [1:0]  phase   = P_SHIFT;
reg [31:0] holdcnt = 32'd0;
reg [15:0] divcnt  = 16'd0;
reg [6:0]  bitcnt  = 7'd0;
reg        sclk    = 1'b0;
reg        sdat    = 1'b0;
reg        g_le    = 1'b0;
reg        g_oe    = 1'b0;

// state[1] selects the register pattern, state[0] the output-enable level, so
// the four experiments are just the two bits counted through.
wire pattern_bit = state[1];
wire oe_level    = state[0];

wire tick = (divcnt == SHIFT_DIV[15:0] - 16'd1);

always @(posedge i_clock_20M) begin
    divcnt <= tick ? 16'd0 : divcnt + 16'd1;

    case (phase)
    P_SHIFT: begin
        // Output enable is held OFF while the pattern is clocked in, so a
        // half-shifted register is never driven onto the socket.
        g_oe <= 1'b0;
        g_le <= 1'b0;
        if (tick) begin
            sclk <= ~sclk;
            if (!sclk) begin              // about to rise: present the data
                sdat <= pattern_bit;
            end else begin                // just fell: count the bit
                bitcnt <= bitcnt + 7'd1;
                if (bitcnt == NBITS[6:0] - 7'd1) begin
                    bitcnt <= 7'd0;
                    phase  <= P_LATCH;
                end
            end
        end
    end
    P_LATCH: begin
        sclk <= 1'b0;
        g_le <= 1'b1;                     // capture the shifted pattern
        if (tick) begin
            g_le    <= 1'b0;
            phase   <= P_HOLD;
            holdcnt <= 32'd0;
        end
    end
    P_HOLD: begin
        g_oe <= oe_level;
        if (holdcnt == HOLD_TICKS[31:0] - 32'd1) begin
            g_oe  <= 1'b0;                // always leave OFF between states
            state <= state + 2'd1;
            phase <= P_SHIFT;
        end else begin
            holdcnt <= holdcnt + 32'd1;
        end
    end
    default: phase <= P_SHIFT;
    endcase
end

assign ser_clk  = sclk;
assign ser_data = sdat;
assign gnd_le   = g_le;
assign gnd_oe   = g_oe;

// --- UART: announce the current state as "Snn\r\n" --------------------------
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
            3'd0:    b = 8'h53;       // 'S'
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

assign uart_tx = tx_bit(8'h30, 8'h30 + {6'd0, state}, ubyte, ubit);

endmodule

`default_nettype wire
