// ---------------------------------------------------------------------------
// T76 bring-up beacon.
//
// Drives all 24 usable ISP header pins and all 48 ZIF pins at once, each one
// continuously transmitting its OWN name at 115200 8N1: "J05\r\n", "Z12\r\n".
//
// The census transmits on one pin, which makes locating that pin guesswork --
// and a wrong guess is indistinguishable from a dead design. Here any pin you
// touch answers with its identity, so a single connection settles both "is the
// FPGA actually running" and "which pin is this".
//
// Every pin transmits in lockstep from one shared sequencer, so per-pin cost is
// a few LUTs: only the two digit characters differ between instances.
//
// No reset network, for the same reason as the census: an outer `if (rst)`
// makes TD infer a set/reset pin per flop and it then refuses to pack flops
// needing both (SYN-8700). Declared initial values are loaded at configuration.
//
// SAFETY: the ZIF socket must be EMPTY. This drives all 48 ZIF pins.
// ---------------------------------------------------------------------------

`default_nettype none

module t76_beacon (
`include "beacon_ports.vh"
);

localparam integer BAUD_DIV = 20_000_000 / 115200;   // 173.6 -> 174

// ISP header: keep 27 and 28 grounded so the probe has a reference.
// All five switchable grounds asserted: the probe's ground lead can land on
// header position 11, 21, 26, 27 or 28. None of these is driven by the FPGA.
assign j_gnd_11 = 1'b1;
assign j_gnd_21 = 1'b1;
assign j_gnd_26 = 1'b1;
assign j_gnd_27 = 1'b1;
assign j_gnd_28 = 1'b1;
assign j_vcc_04 = 1'b0;
assign j_vcc_20 = 1'b0;
assign j_vcc_22 = 1'b0;
assign j_vcc_24 = 1'b0;
assign j_vpp_24 = 1'b0;
assign j_vpp_26 = 1'b0;

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
assign ser_clk  = 1'b0;
assign ser_data = 1'b0;
assign vpp_le   = 1'b0;
assign vcc_le   = 1'b0;
assign gnd_le   = 1'b0;
assign vpp_oe   = 1'b1;   // released (active low)
assign vcc_oe   = 1'b1;   // released (active low)
assign gnd_oe   = 1'b1;   // released (active low)

// --- one shared bit/byte sequencer for every transmitter --------------------
reg [8:0] div      = 9'd0;
reg [3:0] bit_idx  = 4'd0;   // 0 = start, 1..8 = data LSB first, 9 = stop
reg [2:0] byte_idx = 3'd0;   // 0..4 = group, tens, units, CR, LF

always @(posedge i_clock_20M) begin
    if (div == 9'd0) begin
        div <= BAUD_DIV[8:0] - 9'd1;
        if (bit_idx == 4'd9) begin
            bit_idx  <= 4'd0;
            byte_idx <= (byte_idx == 3'd4) ? 3'd0 : byte_idx + 3'd1;
        end else begin
            bit_idx <= bit_idx + 4'd1;
        end
    end else begin
        div <= div - 9'd1;
    end
end

// --- per-pin output: same timing, different characters ----------------------
// byte_idx and bit_idx are passed in rather than read from module scope: a
// continuous assignment that calls a function only re-evaluates when the
// function's ARGUMENTS change. Reading them implicitly leaves every pin frozen
// at its time-zero value -- which simulates as a dead line and would have
// looked exactly like broken hardware.
function tx_bit_for;
    input [7:0] g;      // group character, 'J' or 'Z'
    input [7:0] t;      // tens digit
    input [7:0] u;      // units digit
    input [2:0] byte_i;
    input [3:0] bit_i;
    reg [7:0] b;
    begin
        case (byte_i)
            3'd0:    b = g;
            3'd1:    b = t;
            3'd2:    b = u;
            3'd3:    b = 8'h0D;
            default: b = 8'h0A;
        endcase
        if (bit_i == 4'd0)       tx_bit_for = 1'b0;            // start bit
        else if (bit_i <= 4'd8)  tx_bit_for = b[bit_i - 4'd1];  // data
        else                     tx_bit_for = 1'b1;            // stop bit
    end
endfunction

`include "beacon_drive.vh"

endmodule

`default_nettype wire
