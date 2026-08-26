// ---------------------------------------------------------------------------
// T76 GAL/PAL combinatorial sweep.
//
// A secured GAL still WORKS -- the security fuse blocks readback, not
// operation. So reverse engineering one is not about defeating the fuse; it is
// about characterising behaviour from outside. Drive every input combination,
// record every output, and hand the truth table to a host that can minimise it.
//
// Pin map, GAL16V8 bottom-justified in the 48-pin socket. Derived from this
// project's own measurements -- the socket's DIP numbering, and Z24 being
// permanently grounded and outside the shift chain -- not from the vendor
// pinout:
//
//     chip 1..9  -> ZIF15..ZIF23   driven      (9 inputs)
//     chip 11    -> ZIF25          driven      (1 input, /OE or I)
//     chip 10    -> ZIF24          GND, permanently grounded, nothing to do
//     chip 12..19-> ZIF26..ZIF33   sampled     (8 I/O)
//     chip 20    -> ZIF34          VCC, needs BEGIN_TRANS from the MCU
//
// 10 inputs is 1024 vectors. At 8 cycles per vector that is 410 us of sweeping,
// against 6 ms to report it at 1.84 Mbaud -- the measurement is never the cost.
//
// COMMANDED OVER THE WIRE, not rebuilt. A UART receiver on ISP pin 9 takes
// single-character commands so parameters change without a TD build and a
// physical replug. That loop, not the measurements, has been the dominant cost
// of this work.
//
//     'S'  start a sweep        '?'  status        'H'  halt
//
// SAFETY: rails are RELEASED (active low, so 1). VCC is not routed here -- this
// increment validates the engine and telemetry with an empty socket, and power
// delivery is a separate step with its own risks.
// ---------------------------------------------------------------------------

`default_nettype none

module t76_galsweep (
`include "galsweep_ports.vh"
);

localparam integer CLK_HZ   = 20_000_000;
localparam integer BAUD     = 115200;
localparam integer BAUD_DIV = CLK_HZ / BAUD;      // 173.6 -> 174, 0.2% error
localparam integer NVEC     = 1024;               // 2^10 inputs
localparam integer SETTLE   = 8;                  // cycles: 400 ns at 20 MHz

// Rails released. This experiment does not drive the socket's power network.
assign ser_clk = 1'b0;  assign ser_data = 1'b0;
assign vpp_le  = 1'b0;  assign vcc_le   = 1'b0;  assign gnd_le = 1'b0;
assign vpp_oe  = 1'b1;  assign vcc_oe   = 1'b1;  assign gnd_oe = 1'b1;

// ISP header: 26 is true ground and is asserted so the header has a reference.
// 27 and 28 are left off -- they measured -3 V against the USB-C shell.
assign j_gnd_11 = 1'b1; assign j_gnd_21 = 1'b1; assign j_gnd_26 = 1'b1;
assign j_gnd_27 = 1'b0; assign j_gnd_28 = 1'b0;
assign j_vcc_04 = 1'b0; assign j_vcc_20 = 1'b0; assign j_vcc_22 = 1'b0;
assign j_vcc_24 = 1'b0; assign j_vpp_24 = 1'b0; assign j_vpp_26 = 1'b0;

// --- vector drive and sample ------------------------------------------------
reg [9:0] vec = 10'd0;
assign {drv_11, drv_09, drv_08, drv_07, drv_06,
        drv_05, drv_04, drv_03, drv_02, drv_01} = vec;

wire [7:0] smp = {smp_19, smp_18, smp_17, smp_16,
                  smp_15, smp_14, smp_13, smp_12};

// --- UART transmit ----------------------------------------------------------
reg [8:0] tdiv  = 9'd0;
reg [3:0] tbit  = 4'd15;      // 15 = idle
reg [7:0] tsr   = 8'h00;
wire      tbusy = (tbit != 4'd15);
reg       tsend = 1'b0;
reg [7:0] tdata = 8'h00;

always @(posedge i_clock_20M) begin
    if (!tbusy) begin
        if (tsend) begin tsr <= tdata; tbit <= 4'd0; tdiv <= BAUD_DIV[8:0] - 9'd1; end
    end else if (tdiv == 9'd0) begin
        tdiv <= BAUD_DIV[8:0] - 9'd1;
        tbit <= (tbit == 4'd9) ? 4'd15 : tbit + 4'd1;
    end else tdiv <= tdiv - 9'd1;
end
assign uart_tx = (tbit == 4'd15) ? 1'b1 :
                 (tbit == 4'd0)  ? 1'b0 :
                 (tbit == 4'd9)  ? 1'b1 : tsr[tbit - 4'd1];

// --- UART receive -----------------------------------------------------------
// Sampled at the bit centre, and the start bit is re-checked there: a glitch on
// an idle line would otherwise frame a byte out of noise, which is exactly the
// class of fault that has cost this project the most.
reg [1:0] rsync = 2'b11;
reg [8:0] rdiv  = 9'd0;
reg [3:0] rbit  = 4'd15;
reg [7:0] rsr   = 8'h00;
reg [7:0] rbyte = 8'h00;
reg       rgot  = 1'b0;

always @(posedge i_clock_20M) begin
    rsync <= {rsync[0], uart_rx};
    rgot  <= 1'b0;
    if (rbit == 4'd15) begin
        if (!rsync[1]) begin rbit <= 4'd0; rdiv <= (BAUD_DIV[8:0] >> 1); end
    end else if (rdiv == 9'd0) begin
        rdiv <= BAUD_DIV[8:0] - 9'd1;
        if (rbit == 4'd0) begin
            if (rsync[1]) rbit <= 4'd15;        // not a real start bit
            else          rbit <= 4'd1;
        end else if (rbit <= 4'd8) begin
            rsr  <= {rsync[1], rsr[7:1]};
            rbit <= rbit + 4'd1;
        end else begin
            if (rsync[1]) begin rbyte <= rsr; rgot <= 1'b1; end
            rbit <= 4'd15;
        end
    end else rdiv <= rdiv - 9'd1;
end

// --- results ----------------------------------------------------------------
reg [7:0] mem [0:NVEC-1];
reg [7:0] chg = 8'h00;        // sampled pins that ever changed: direction discovery for free
reg [7:0] first = 8'h00;
reg       have_first = 1'b0;

// --- sweep + report FSM -----------------------------------------------------
localparam [3:0] S_IDLE=0, S_BANNER=1, S_SET=2, S_WAIT=3, S_STORE=4,
                 S_HDR=5, S_ROW=6, S_END=7, S_EMIT=8;

reg [3:0]  st = S_BANNER, ret = S_IDLE;
reg [7:0]  settle = 8'd0;
reg [10:0] idx = 11'd0;
reg [5:0]  col = 6'd0;
reg [5:0]  ci  = 6'd0;
reg [7:0]  msg [0:47];
reg [5:0]  mlen = 6'd0;

function [7:0] hex; input [3:0] n; hex = (n < 4'd10) ? (8'h30 + n) : (8'h37 + n); endfunction

integer k;
always @(posedge i_clock_20M) begin
    tsend <= 1'b0;

    case (st)
    S_BANNER: begin
        msg[0]<="G"; msg[1]<="A"; msg[2]<="L"; msg[3]<="S"; msg[4]<="W"; msg[5]<="E";
        msg[6]<="E"; msg[7]<="P"; msg[8]<=" "; msg[9]<="1"; msg[10]<=8'h0D; msg[11]<=8'h0A;
        mlen <= 6'd12; ci <= 6'd0; ret <= S_IDLE; st <= S_EMIT;
    end

    S_IDLE: begin
        if (rgot) begin
            if (rbyte == "S") begin
                vec <= 10'd0; idx <= 11'd0; chg <= 8'h00; have_first <= 1'b0;
                settle <= 8'd0; st <= S_SET;
            end else if (rbyte == "?") begin
                msg[0]<="O"; msg[1]<="K"; msg[2]<=8'h0D; msg[3]<=8'h0A;
                mlen <= 6'd4; ci <= 6'd0; ret <= S_IDLE; st <= S_EMIT;
            end
        end
    end

    S_SET:   begin settle <= 8'd0; st <= S_WAIT; end
    S_WAIT:  begin
        // Hold the vector and let the part settle before looking. A GAL16V8-25
        // is 25 ns; 400 ns is an order of magnitude of margin, and the socket's
        // driver network bandwidth has never been characterised.
        if (settle == SETTLE[7:0] - 8'd1) st <= S_STORE; else settle <= settle + 8'd1;
    end
    S_STORE: begin
        mem[idx[9:0]] <= smp;
        if (!have_first) begin first <= smp; have_first <= 1'b1; end
        else             chg <= chg | (smp ^ first);
        if (idx == NVEC[10:0] - 11'd1) begin idx <= 11'd0; col <= 6'd0; st <= S_HDR; end
        else begin idx <= idx + 11'd1; vec <= vec + 10'd1; st <= S_SET; end
    end

    S_HDR: begin
        msg[0]<="D"; msg[1]<=hex(idx[9:8]); msg[2]<=hex(idx[7:4]); msg[3]<=hex(idx[3:0]);
        msg[4]<=" "; mlen <= 6'd5; ci <= 6'd0; ret <= S_ROW; st <= S_EMIT;
    end
    S_ROW: begin
        // 16 bytes a line: short enough that a dropped line is obvious against
        // its neighbours, long enough that the framing overhead is small.
        msg[0]<=hex(mem[idx[9:0]][7:4]); msg[1]<=hex(mem[idx[9:0]][3:0]); msg[2]<=" ";
        mlen <= 6'd3; ci <= 6'd0;
        if (col == 6'd15) begin
            msg[2]<=8'h0D; msg[3]<=8'h0A; mlen <= 6'd4; col <= 6'd0;
            ret <= (idx == NVEC[10:0]-11'd1) ? S_END : S_HDR;
        end else begin
            col <= col + 6'd1; ret <= S_ROW;
        end
        idx <= idx + 11'd1;
        st  <= S_EMIT;
    end
    S_END: begin
        // chg is the direction discovery, for free: a sampled pin that never
        // moved across all 1024 vectors is an input or an unused output, and
        // the host needs to know which of the eight actually carry a function.
        msg[0]<="E"; msg[1]<="N"; msg[2]<="D"; msg[3]<=" ";
        msg[4]<="c"; msg[5]<="h"; msg[6]<="g"; msg[7]<="=";
        msg[8]<=hex(chg[7:4]); msg[9]<=hex(chg[3:0]); msg[10]<=8'h0D; msg[11]<=8'h0A;
        mlen <= 6'd12; ci <= 6'd0; ret <= S_IDLE; st <= S_EMIT;
    end

    S_EMIT: begin
        if (!tbusy && !tsend) begin
            if (ci == mlen) st <= ret;
            else begin tdata <= msg[ci]; tsend <= 1'b1; ci <= ci + 6'd1; end
        end
    end
    default: st <= S_IDLE;
    endcase
end

endmodule

`default_nettype wire
