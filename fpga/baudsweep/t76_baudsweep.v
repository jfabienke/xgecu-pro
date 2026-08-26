// ---------------------------------------------------------------------------
// T76 UART baud sweep: find the ceiling of the ISP-header serial path.
//
// The readback rate is the binding constraint on every tool built on this
// header -- the PAL/GAL sweep resolves in milliseconds and then spends seconds
// getting the answer out. So the useful number is not "what can the FPGA
// generate" (anything) but "what survives the path to a header pin and a
// jumper, and still decodes at the other end".
//
// One bitstream, six rates, rather than six builds. Each step transmits its own
// index continuously as "Bnn\r\n" for about two seconds, so the receiver can
// tell which rate it is looking at from the content, and can measure the bit
// period from the waveform whether or not the frame decodes.
//
//   step  divisor   baud
//     0     174     114,943   the proven rate
//     1      87     229,885
//     2      43     465,116
//     3      22     909,091
//     4      11   1,818,182
//     5       5   4,000,000
//
// Divisors are exact integers at 20 MHz, so each rate is what it says rather
// than a rounded approximation -- a decode failure is then the path failing,
// not the transmitter drifting.
//
// SAFETY: drives one ISP header pin and the rail controls, nothing else. The
// ZIF socket is untouched, and the rail enables are RELEASED (active low = 1).
// ---------------------------------------------------------------------------

`default_nettype none

module t76_baudsweep (
`include "baudsweep_ports.vh"
);

localparam integer NSTEP     = 6;
localparam integer HOLD      = 20_000_000 * 2;   // ~2 s per rate

// ISP header power/ground. 26 measured at true ground and is asserted so the
// header keeps a reference; 27 and 28 are NOT asserted -- with the beacon
// asserting them they measured -3 V against the USB-C shell, and there is no
// reason to energize an undocumented negative rail during a timing experiment.
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

// Rails released throughout; this experiment does not touch the socket.
assign ser_clk = 1'b0;
assign ser_data = 1'b0;
assign vpp_le = 1'b0;
assign vcc_le = 1'b0;
assign gnd_le = 1'b0;
assign vpp_oe = 1'b1;
assign vcc_oe = 1'b1;
assign gnd_oe = 1'b1;

// --- which rate we are on ---------------------------------------------------
reg [2:0]  step    = 3'd0;
reg [31:0] holdcnt = 32'd0;

always @(posedge i_clock_20M) begin
    if (holdcnt == HOLD[31:0] - 32'd1) begin
        holdcnt <= 32'd0;
        step    <= (step == NSTEP[2:0] - 3'd1) ? 3'd0 : step + 3'd1;
    end else begin
        holdcnt <= holdcnt + 32'd1;
    end
end

// Divisor for the current step. A case rather than a table so the synthesiser
// sees constants and does not infer a memory for six values.
reg [8:0] bdiv;
always @(*) begin
    case (step)
        3'd0:    bdiv = 9'd174;   //   114,943
        3'd1:    bdiv = 9'd87;    //   229,885
        3'd2:    bdiv = 9'd43;    //   465,116
        3'd3:    bdiv = 9'd22;    //   909,091
        3'd4:    bdiv = 9'd11;    // 1,818,182
        default: bdiv = 9'd5;     // 4,000,000
    endcase
end

// --- transmitter ------------------------------------------------------------
reg [8:0] div   = 9'd0;
reg [3:0] bitix = 4'd0;   // 0 start, 1..8 data LSB first, 9 stop
reg [2:0] byix  = 3'd0;   // 0 'B', 1 tens, 2 units, 3 CR, 4 LF

always @(posedge i_clock_20M) begin
    if (div == 9'd0) begin
        div <= bdiv - 9'd1;
        if (bitix == 4'd9) begin
            bitix <= 4'd0;
            byix  <= (byix == 3'd4) ? 3'd0 : byix + 3'd1;
        end else begin
            bitix <= bitix + 4'd1;
        end
    end else begin
        div <= div - 9'd1;
    end
end

// Arguments passed in, not read from module scope: a continuous assignment
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
            3'd0:    b = 8'h42;      // 'B'
            3'd1:    b = t;
            3'd2:    b = u;
            3'd3:    b = 8'h0D;
            default: b = 8'h0A;
        endcase
        case (bi)
            4'd0:    tx_bit = 1'b0;
            4'd9:    tx_bit = 1'b1;
            default: tx_bit = b[bi - 4'd1];
        endcase
    end
endfunction

assign uart_tx = tx_bit(8'h30, 8'h30 + {5'd0, step}, byix, bitix);

endmodule

`default_nettype wire
