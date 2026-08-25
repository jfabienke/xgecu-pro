// ---------------------------------------------------------------------------
// T76 VCC probe: one chain bit, into the VCC latch, held.
//
// Two questions, one experiment, because they turn out to be the same one.
//
//   1. Is vcc_oe active low, like gnd_oe is?
//   2. What are Z23 and Z24? They are pulled up, in no way ground-switchable,
//      and chain bits 16 and 21 drive no GND position at all.
//
// If the three domains share one shift register -- which is what separate
// vcc_le / vpp_le / gnd_le imply -- then the same bit number addresses the same
// socket position in every domain. So asserting bit 16 or 21 into the VCC latch
// and watching Z23/Z24 answers the second question with the machinery built for
// the first. Supply-only positions would explain everything: not switchable to
// ground because they were never meant to be.
//
// SELECT_BIT picks which chain bit is driven. Default 22, which is the safest
// possible target: bit 22 -> Z22 is already confirmed in the GND domain, so a
// null result is unambiguous rather than "maybe the wrong position".
//
// ONE BIT, NOT ALL-ONES. The ground experiments could safely assert everything
// because grounding a socket pin is harmless. This one sources VCC, so exactly
// one position is energized, at a known location.
//
// SAFETY:
//   - THE ZIF SOCKET MUST BE EMPTY. This puts VCC on a socket contact. That is
//     the programmer doing its normal job, and it is only dangerous if
//     something 3.3 V is sitting in there.
//   - VPP is untouched: vpp_le and vpp_oe stay released for the whole of this.
//     VCC is ~5 V and VPP is 12 V+, so the mechanism gets established at the
//     lower voltage first.
//   - The ZIF pins are not declared, so the FPGA drives nothing into the socket.
//
// The instrument here is a METER, not the Pico -- the Pico is out of the socket
// and 3.3 V logic has no business near a 5 V rail. Given how often plausible
// inference has been wrong in this work, an independent direct reading is the
// point rather than a fallback.
// ---------------------------------------------------------------------------

`default_nettype none

module t76_vccprobe (
`include "vccprobe_ports.vh"
);

localparam integer BAUD_DIV   = 20_000_000 / 115200;
localparam integer SHIFT_DIV  = 64;
localparam integer NBITS      = 48;
// All-ones rather than one bit. After a null result the priority is maximum
// signal: every position energized at once, so nothing depends on having picked
// the right one to probe.
//
// Two states, deliberately ASYMMETRIC in duration, which is what lets a meter
// alone identify them with no UART jumper and no correlating of timestamps:
//
//     vcc_oe = 0   held  4 s   SHORT
//     vcc_oe = 1   held 16 s   LONG
//
// If the brief excursion carries the voltage, the enable is active low like
// gnd_oe. If the long one does, it is active high. If NEITHER produces anything
// above the ~3.1 V resting pull-up level, that is the third answer and a real
// one: the FPGA does not control the VCC supply by itself and something
// MCU-side gates it. Worth establishing before spending more builds on polarity.
localparam integer HOLD_SHORT = 20_000_000 * 4;    // vcc_oe = 0
localparam integer HOLD_LONG  = 20_000_000 * 16;   // vcc_oe = 1

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

// GND domain untouched throughout: this experiment is about VCC.
assign gnd_le = 1'b0;
assign gnd_oe = 1'b1;      // released (active low)
// VPP stays released. 12 V+ is not part of this experiment.
assign vpp_le = 1'b0;
assign vpp_oe = 1'b1;      // released (active low)
// vcc_le and vcc_oe are driven by the state machine below -- NOT here. An
// earlier revision carried a stale "assign vcc_oe = 1'b0" over from a copied
// block, giving that wire two continuous drivers. Verilog resolves that to X,
// and vcc_oe is the one signal in this design that must not be ambiguous.

// --- shift one bit in, latch into VCC, assert and hold ----------------------
localparam [1:0] P_SHIFT = 2'd0, P_LATCH = 2'd1, P_HOLD = 2'd2;

reg [1:0]  phase  = P_SHIFT;
reg [15:0] divcnt = 16'd0;
reg [7:0]  bitcnt = 8'd0;
reg        sclk   = 1'b0;
reg        v_le   = 1'b0;
reg        v_oe   = 1'b1;      // released until the register is fully loaded
reg        oe_sel = 1'b0;      // which of the two states we are in
reg [31:0] holdcnt = 32'd0;

wire tick = (divcnt == SHIFT_DIV[15:0] - 16'd1);

always @(posedge i_clock_20M) begin
    divcnt <= tick ? 16'd0 : divcnt + 16'd1;
    case (phase)
    P_SHIFT: begin
        v_oe <= 1'b1;                 // never drive a half-shifted register
        v_le <= 1'b0;
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
        v_le <= 1'b1;
        if (tick) begin
            v_le  <= 1'b0;
            phase <= P_HOLD;
        end
    end
    default: begin                    // P_HOLD -- cycles the two OE states
        v_oe <= oe_sel;
        if (holdcnt >= (oe_sel ? HOLD_LONG[31:0] : HOLD_SHORT[31:0]) - 32'd1) begin
            holdcnt <= 32'd0;
            oe_sel  <= ~oe_sel;
        end else begin
            holdcnt <= holdcnt + 32'd1;
        end
    end
    endcase
end

assign ser_clk  = sclk;
assign ser_data = 1'b1;                          // all ones
assign vcc_le   = v_le;
assign vcc_oe   = v_oe;

// --- UART: announce a fixed "V22\r\n" so the tool can see the probe is running --------------------------
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
            3'd0:    b = 8'h56;       // 'V'
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

assign uart_tx = tx_bit(8'h39, 8'h39, ubyte, ubit);   // "V99"

endmodule

`default_nettype wire
