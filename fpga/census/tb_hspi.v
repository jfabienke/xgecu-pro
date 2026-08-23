// Drives a real HSPI burst at the census and checks that the capture path
// recovers the exact words the "MCU" clocked out.
//
// This is the test that has to pass before the design is trusted on hardware:
// the whole point of stage 2 is reading bytes off that bus, and a capture that
// silently returns zeros looks identical to a link that carries nothing.
`timescale 1ns/1ps
`default_nettype none

module tb_hspi;
`include "census_params.vh"
`include "census_tb_idx.vh"
    localparam integer HDR      = 8;
    localparam integer EDGE_OFF = HDR + 3*NBYTES;
    localparam integer STAT_OFF = EDGE_OFF + 2*NDETAIL;
    localparam integer CAP_OFF  = STAT_OFF + 6;
    localparam integer CAPN     = 32;
    localparam integer CAPDEPTH = 32;
    localparam integer FRAME_LEN = CAP_OFF + 4*CAPN + 2;
    localparam integer BIT_NS   = 174*50;

    reg clk = 1'b0;
    always #25 clk = ~clk;                   // 20 MHz sampling clock

    reg [NPINS-1:0] tb_obs = {NPINS{1'b0}};
    wire uart_tx, htrdy;
    wire ser_clk, ser_data, vpp_le, vcc_le, gnd_le, vpp_oe, vcc_oe, gnd_oe;
    wire j_gnd_11, j_gnd_21, j_gnd_26, j_gnd_27, j_gnd_28;
    wire j_vcc_04, j_vcc_20, j_vcc_22, j_vcc_24, j_vpp_24, j_vpp_26;

    t76_census #(.FRAME_GAP(200_000), .CAPDEPTH(CAPDEPTH)) dut (
`include "census_tb_connect.vh"
        .i_clock_20M(clk), .uart_tx(uart_tx), .htrdy(htrdy),
        .ser_clk(ser_clk), .ser_data(ser_data), .vpp_le(vpp_le), .vcc_le(vcc_le),
        .gnd_le(gnd_le), .vpp_oe(vpp_oe), .vcc_oe(vcc_oe), .gnd_oe(gnd_oe),
        .j_gnd_11(j_gnd_11), .j_gnd_21(j_gnd_21), .j_gnd_26(j_gnd_26),
        .j_gnd_27(j_gnd_27), .j_gnd_28(j_gnd_28), .j_vcc_04(j_vcc_04),
        .j_vcc_20(j_vcc_20), .j_vcc_22(j_vcc_22), .j_vcc_24(j_vcc_24),
        .j_vpp_24(j_vpp_24), .j_vpp_26(j_vpp_26)
    );

    // free-running HTCLK at 100 MHz, as the MCU drives it
    always #5 tb_obs[IDX_HTCLK] = ~tb_obs[IDX_HTCLK];

    integer errors = 0;
    task check(input cond, input [255:0] what);
        begin if (!cond) begin $display("  FAIL: %0s", what); errors = errors + 1; end end
    endtask

    // put a 24-bit word on the HD lines
    task drive_hd(input [23:0] w);
        begin
`include "census_tb_drive.vh"
        end
    endtask

    reg [23:0] sent [0:31];
    integer n;

    reg [7:0]  frame [0:FRAME_LEN-1];
    reg [31:0] capw;
    reg [15:0] capwords, bursts;
    integer fi, bi;


    initial begin
        // --- the MCU side of a single packet ---------------------------------
        tb_obs[IDX_HTREQ] = 1'b0;
        tb_obs[IDX_HTVLD] = 1'b0;
        #2000;

        tb_obs[IDX_HTREQ] = 1'b1;            // "there is data to transmit"
        #500;
        check(htrdy === 1'b1, "htrdy asserts after HTREQ");

        // known payload: a header-ish word then a counting pattern
        for (n = 0; n < 32; n = n + 1) sent[n] = 24'h010000 + n*24'h010101;
        sent[0] = 24'h0000A5; sent[4] = 24'hFFFFFF; sent[5] = 24'h555555;
        sent[6] = 24'hAAAAAA; sent[7] = 24'h123456;

        @(negedge tb_obs[IDX_HTCLK]);
        tb_obs[IDX_HTVLD] = 1'b1;
        for (n = 0; n < 32; n = n + 1) begin
            drive_hd(sent[n]);
            @(posedge tb_obs[IDX_HTCLK]);    // the DUT samples here
            #1;
        end
        @(negedge tb_obs[IDX_HTCLK]);
        tb_obs[IDX_HTVLD] = 1'b0;
        tb_obs[IDX_HTREQ] = 1'b0;
        #1000;
        check(htrdy === 1'b0, "htrdy releases when HTREQ drops");
        $display("  [probe] capcnt=%0d capwords=%0d bursts=%0d",
                 dut.capcnt, dut.capwords, dut.bursts);
        for (n = 0; n < 4; n = n + 1)
            $display("  [probe] capbuf[%0d]=%06h  sent=%06h",
                     n, dut.capbuf[n][23:0], sent[n]);

        // --- read the frame back over the UART -------------------------------
        read_frame;
        for (n = 0; n < 32; n = n + 1) begin
            capw = {frame[CAP_OFF+4*n+3], frame[CAP_OFF+4*n+2],
                    frame[CAP_OFF+4*n+1], frame[CAP_OFF+4*n]};
            // HD17 is program_b and permanently tied low in hd_bus, so it can
            // never carry data. Mask it rather than expect it.
            if ((capw[23:0] & ~(24'd1 << 17)) !== (sent[n] & ~(24'd1 << 17))) begin
                $display("  FAIL: word %0d captured %06h expected %06h",
                         n, capw[23:0], sent[n]);
                errors = errors + 1;
            end
        end
        capwords = {frame[STAT_OFF+1], frame[STAT_OFF]};
        bursts   = {frame[STAT_OFF+3], frame[STAT_OFF+2]};
        check(capwords == 16'd32, "capwords counted 32");
        check(frame[STAT_OFF+5] == 8'd1, "one-shot reports frozen");
        check(bursts   == 16'd1, "one burst seen");
        check(frame[4] == 8'h04, "version 4");

        if (errors == 0) $display("  HSPI capture: all checks passed (%0d-byte frame)", FRAME_LEN);
        else             $display("  %0d FAILURES", errors);
        $finish;
    end

    task read_frame;
        reg [7:0] b;
        integer got;
        begin
            got = 0;
            // resync: find the preamble, then take a whole frame
            while (got < 2) begin
                rx_byte(b);
                if (b == 8'h55) begin rx_byte(b);
                    if (b == 8'hAA) begin rx_byte(b);
                        if (b == 8'h55) begin rx_byte(b);
                            if (b == 8'hAA) got = 2; end end end
            end
            frame[0]=8'h55; frame[1]=8'hAA; frame[2]=8'h55; frame[3]=8'hAA;
            for (fi = 4; fi < FRAME_LEN; fi = fi + 1) begin rx_byte(b); frame[fi] = b; end
        end
    endtask

    task rx_byte(output [7:0] out);
        begin
            @(negedge uart_tx);
            #(BIT_NS + BIT_NS/2);
            for (bi = 0; bi < 8; bi = bi + 1) begin out[bi] = uart_tx; #(BIT_NS); end
        end
    endtask

    initial begin #200_000_000; $display("  TIMEOUT"); $finish; end
endmodule
`default_nettype wire
