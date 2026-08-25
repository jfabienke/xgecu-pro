// Verifies the FPGA->MCU transmitter, and above all the bus interlock.
//
// HD is shared. If the FPGA ever drives it while the MCU is driving, that is
// contention on a live board -- the one failure here that damages hardware
// rather than just producing wrong data. The testbench releases HD whenever the
// MCU is not driving, so any overlap shows up as X on the net and is caught.
`timescale 1ns/1ps
`default_nettype none

module tb_txrx;
`include "probe_params.vh"
`include "probe_tb_idx.vh"
    localparam integer HDR      = 8;
    localparam integer BIT_NS   = 174*50;

    reg clk = 1'b0;
    always #25 clk = ~clk;

    reg  [NPINS-1:0] tb_obs = {NPINS{1'b0}};
    reg              mcu_drive = 1'b1;
    wire [NPINS-1:0] obs_net;
`include "census_tb_net.vh"
`include "census_tb_read.vh"

    wire uart_tx, htrdy, hrclk, hract, hrvld;
    wire ser_clk, ser_data, vpp_le, vcc_le, gnd_le, vpp_oe, vcc_oe, gnd_oe;
    wire j_gnd_11, j_gnd_21, j_gnd_26, j_gnd_27, j_gnd_28;
    wire j_vcc_04, j_vcc_20, j_vcc_22, j_vcc_24, j_vpp_24, j_vpp_26;

    t76_hspi_probe #(.FRAME_GAP(100_000)) dut (
`include "probe_tb_connect.vh"
        .i_clock_20M(clk), .uart_tx(uart_tx), .htrdy(htrdy),
        .hrclk(hrclk), .hract(hract), .hrvld(hrvld),
        .ser_clk(ser_clk), .ser_data(ser_data), .vpp_le(vpp_le), .vcc_le(vcc_le),
        .gnd_le(gnd_le), .vpp_oe(vpp_oe), .vcc_oe(vcc_oe), .gnd_oe(gnd_oe),
        .j_gnd_11(j_gnd_11), .j_gnd_21(j_gnd_21), .j_gnd_26(j_gnd_26),
        .j_gnd_27(j_gnd_27), .j_gnd_28(j_gnd_28), .j_vcc_04(j_vcc_04),
        .j_vcc_20(j_vcc_20), .j_vcc_22(j_vcc_22), .j_vcc_24(j_vcc_24),
        .j_vpp_24(j_vpp_24), .j_vpp_26(j_vpp_26)
    );

    always #5 tb_obs[IDX_HTCLK] = ~tb_obs[IDX_HTCLK];

    integer errors = 0, n, contention = 0;
    reg [15:0] got [0:23];
    integer ngot = 0;

    task check(input cond, input [255:0] what);
        begin if (!cond) begin $display("  FAIL: %0s", what); errors = errors + 1; end end
    endtask

    task drive_hd(input [23:0] w);
        begin
`include "probe_tb_drive.vh"
        end
    endtask

    // Contention is precisely "both ends enabled at once". Watching the net for
    // X also flags a *floating* bus, because ^z is x too -- which is what the
    // first version of this check did, reporting a fault when nobody was driving.
    always @(posedge clk)
        if (mcu_drive && dut.hd_drive) begin
            contention = contention + 1;
            if (contention < 4)
                $display("  [contention] t=%0t HTVLD=%b tx_state=%0d",
                         $time, tb_obs[IDX_HTVLD], dut.tx_state);
        end

    // capture whatever the DUT clocks out while it owns the bus
    always @(posedge hrclk)
        if (hrvld && !mcu_drive && ngot < 24) begin
            got[ngot] = hd_read;
            ngot = ngot + 1;
        end

    initial begin
        tb_obs[IDX_HTREQ] = 1'b0; tb_obs[IDX_HTVLD] = 1'b0;
        #3000;

        // --- MCU sends a packet ---------------------------------------------
        tb_obs[IDX_HTREQ] = 1'b1;
        #400;
        check(htrdy === 1'b1, "htrdy asserts for the MCU's request");
        @(negedge tb_obs[IDX_HTCLK]);
        tb_obs[IDX_HTVLD] = 1'b1;
        for (n = 0; n < 6; n = n + 1) begin
            drive_hd(24'h000100 + n);
            @(posedge tb_obs[IDX_HTCLK]); #1;
        end
        @(negedge tb_obs[IDX_HTCLK]);
        tb_obs[IDX_HTVLD] = 1'b0;
        tb_obs[IDX_HTREQ] = 1'b0;

        // --- the MCU releases the bus and waits for our reply ----------------
        #500;
        mcu_drive = 1'b0;                       // MCU stops driving HD
        fork : w1
            begin wait (hract === 1'b1); disable w1; end
            begin #200_000; check(1'b0, "DUT never raised HRACT"); disable w1; end
        join
        if (hract === 1'b1) $display("  DUT raised HRACT");
        #300;
        tb_obs[IDX_HTACK] = 1'b1;               // MCU says ready
        fork : w2
            begin wait (hrvld === 1'b1); disable w2; end
            begin #200_000; check(1'b0, "DUT never raised HRVLD"); disable w2; end
        join
        if (hrvld === 1'b1) $display("  DUT raised HRVLD and is transmitting");
        fork : w3
            begin wait (hrvld === 1'b0); disable w3; end
            begin #200_000; disable w3; end
        join
        tb_obs[IDX_HTACK] = 1'b0;
        mcu_drive = 1'b1;                       // MCU takes the bus back
        #2000;

        check(ngot == 19, "transmitted a 19-word packet");
        // payload words must be a contiguous counter starting at 0
        for (n = 2; n < 18; n = n + 1)
            check(got[n] == (n - 2), "payload counter in sequence");
        check(contention == 0, "NEVER drove HD while the MCU was driving");
        $display("  words out: %04X %04X | %04X %04X %04X %04X %04X %04X | crc %04X",
                 got[0], got[1], got[2], got[3], got[4], got[5], got[6], got[7], got[18]);

        // --- interlock under fire: MCU asserts HTVLD mid-reply ---------------
        ngot = 0;
        mcu_drive = 1'b0;
        #4000;
        fork : w4
            begin wait (hrvld === 1'b1 || hract === 1'b1); disable w4; end
            begin #200_000; disable w4; end
        join
        // A real MCU asserts HTVLD first and only then takes the data lines;
        // driving both in the same instant tests a race no hardware produces.
        tb_obs[IDX_HTVLD] = 1'b1;               // MCU seizes the bus
        #100;                                    // DUT releases on the raw pin
        mcu_drive = 1'b1;
        #2000;
        check(contention == 0, "released HD immediately when HTVLD went high");
        tb_obs[IDX_HTVLD] = 1'b0;

        if (errors == 0) $display("  transmitter: all checks passed");
        else             $display("  %0d FAILURES", errors);
        $finish;
    end
    initial begin #1_000_000; $display("  TIMEOUT"); $finish; end
endmodule
`default_nettype wire
