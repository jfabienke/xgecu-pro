// Testbench for t76_census: drives known pin patterns, decodes the UART frame
// the way decode_census.py will, and checks the three-bit-per-pin semantics.
`timescale 1ns/1ps
`default_nettype none

module tb_census;
`include "census_params.vh"
    localparam integer CAPN      = 32;
    localparam integer HDR       = 8;
    localparam integer EDGE_OFF  = HDR + 3*NBYTES;
    localparam integer STAT_OFF  = EDGE_OFF + 2*NDETAIL;
    localparam integer CAP_OFF   = STAT_OFF + 10;
    localparam integer FRAME_LEN = CAP_OFF + 4*CAPN + 2;
    localparam integer BIT_NS = 174*50;   // 174 clocks of 50 ns

    reg clk = 1'b0;
    always #25 clk = ~clk;                // 20 MHz

    reg [NPINS-1:0] tb_obs = {NPINS{1'b0}};
    reg             mcu_drive = 1'b1;
    wire [NPINS-1:0] obs_net;
`include "census_tb_net.vh"
    wire uart_tx, htrdy;
    wire ser_clk, ser_data, vpp_le, vcc_le, gnd_le, vpp_oe, vcc_oe, gnd_oe;

    t76_census #(.FRAME_GAP(3000)) dut (
`include "census_tb_connect.vh"
        .i_clock_20M (clk),
        .uart_tx     (uart_tx),
        .htrdy       (htrdy),
        .ser_clk     (ser_clk),
        .ser_data    (ser_data),
        .vpp_le      (vpp_le),
        .vcc_le      (vcc_le),
        .gnd_le      (gnd_le),
        .vpp_oe      (vpp_oe),
        .vcc_oe      (vcc_oe),
        .gnd_oe      (gnd_oe)
    );

    // pins 0..9 tied low, 10..19 tied high, 20..29 toggling, rest low
    initial begin
        tb_obs = {NPINS{1'b0}};
        tb_obs[19:10] = 10'h3FF;
    end
    always #200 tb_obs[29:20] = ~tb_obs[29:20];

    // ---- UART receiver ----------------------------------------------------
    reg [7:0] frame [0:FRAME_LEN-1];
    integer   nb;
    reg [7:0] b;
    integer   i;

    task rx_byte(output [7:0] out);
        integer k;
        begin
            @(negedge uart_tx);            // start bit
            #(BIT_NS + BIT_NS/2);          // into the middle of bit 0
            for (k = 0; k < 8; k = k + 1) begin
                out[k] = uart_tx;
                #(BIT_NS);
            end
        end
    endtask

    function [15:0] crc16_step;
        input [15:0] c;
        input [7:0]  d;
        integer j;
        reg [15:0] x;
        begin
            x = c ^ {d, 8'h00};
            for (j = 0; j < 8; j = j + 1)
                x = x[15] ? ((x << 1) ^ 16'h1021) : (x << 1);
            crc16_step = x;
        end
    endfunction

    // ---- capture one complete frame, then verify --------------------------
    integer errors = 0;
    reg [15:0] crc;
    reg [NPINS-1:0] snap, lo, hi;
    reg [15:0] ec;
    integer idx, bit_in_byte;

    task check(input cond, input [255:0] what);
        begin
            if (!cond) begin
                $display("  FAIL: %0s", what);
                errors = errors + 1;
            end
        end
    endtask

    initial begin
        // Skip the first frame (partial window), capture the second.
        nb = 0;
        while (nb < FRAME_LEN) begin rx_byte(b); nb = nb + 1; end

        // resync on preamble for the next frame
        nb = 0;
        while (nb < FRAME_LEN) begin
            rx_byte(b);
            frame[nb] = b;
            nb = nb + 1;
        end

        $display("T76 census testbench");
        check(frame[0]==8'h55 && frame[1]==8'hAA &&
              frame[2]==8'h55 && frame[3]==8'hAA, "preamble");
        check(frame[4]==8'h04, "version == 4");
        check(frame[5]==NPINS, "pin count matches");
        check(frame[6]==NDETAIL, "detail count matches");
        check(frame[7]==CAPN, "capture depth matches");

        crc = 16'hFFFF;
        for (i = 4; i < CAP_OFF + 4*CAPN; i = i + 1) crc = crc16_step(crc, frame[i]);
        check(frame[FRAME_LEN-2]==crc[15:8] && frame[FRAME_LEN-1]==crc[7:0], "CRC-16");

        for (i = 0; i < NPINS; i = i + 1) begin
            idx = i / 8; bit_in_byte = i % 8;
            snap[i] = frame[HDR            + idx][bit_in_byte];
            lo  [i] = frame[HDR +   NBYTES + idx][bit_in_byte];
            hi  [i] = frame[HDR + 2*NBYTES + idx][bit_in_byte];
        end

        for (i = 0; i <= 9; i = i + 1)
            check(lo[i]===1'b1 && hi[i]===1'b0, "pins 0-9 classify as tied low");
        for (i = 10; i <= 19; i = i + 1)
            check(lo[i]===1'b0 && hi[i]===1'b1, "pins 10-19 classify as tied high");
        for (i = 20; i <= 29; i = i + 1)
            check(lo[i]===1'b1 && hi[i]===1'b1, "pins 20-29 classify as active");
        for (i = 30; i < NPINS; i = i + 1)
            check(lo[i]===1'b1 && hi[i]===1'b0, "remaining pins classify as tied low");

        // pins 20..29 toggle every 200 ns, so their counters must be non-zero;
        // pins 0..9 are tied low and must have counted nothing.
        for (i = 20; i <= 29; i = i + 1) begin
            ec = {frame[EDGE_OFF + 2*i + 1], frame[EDGE_OFF + 2*i]};
            check(ec != 16'd0, "toggling pin counted edges");
        end
        for (i = 0; i <= 9; i = i + 1) begin
            ec = {frame[EDGE_OFF + 2*i + 1], frame[EDGE_OFF + 2*i]};
            check(ec == 16'd0, "static pin counted no edges");
        end

        check(ser_clk===1'b0 && ser_data===1'b0 && vpp_oe===1'b0 &&
              vcc_oe===1'b0 && gnd_oe===1'b0, "rail control held in the safe state");

        if (errors == 0) $display("  all checks passed (%0d bytes/frame)", FRAME_LEN);
        else             $display("  %0d FAILURES", errors);
        $finish;
    end

    initial begin
        #50_000_000;
        $display("  TIMEOUT - no complete frame");
        $finish;
    end
endmodule

`default_nettype wire
