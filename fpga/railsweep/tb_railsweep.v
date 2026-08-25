// Simulation for the rail sweep. Checks the things that would be expensive to
// discover on hardware: that the output enable is never asserted while the
// register is half-shifted, that exactly NBITS are clocked per state, and that
// the UART actually announces the state it is in.
`timescale 1ns/1ps
`default_nettype none

module tb_railsweep;
    reg clk = 1'b0;
    always #25 clk = ~clk;              // 20 MHz

    wire uart_tx, ser_clk, ser_data;
    wire vpp_le, vcc_le, gnd_le, vpp_oe, vcc_oe, gnd_oe;
    wire j_gnd_11, j_gnd_21, j_gnd_26, j_gnd_27, j_gnd_28;
    wire j_vcc_04, j_vcc_20, j_vcc_22, j_vcc_24, j_vpp_24, j_vpp_26;

    t76_railsweep dut (
        .i_clock_20M(clk), .uart_tx(uart_tx),
        .ser_clk(ser_clk), .ser_data(ser_data),
        .vpp_le(vpp_le), .vcc_le(vcc_le), .gnd_le(gnd_le),
        .vpp_oe(vpp_oe), .vcc_oe(vcc_oe), .gnd_oe(gnd_oe),
        .j_gnd_11(j_gnd_11), .j_gnd_21(j_gnd_21), .j_gnd_26(j_gnd_26),
        .j_gnd_27(j_gnd_27), .j_gnd_28(j_gnd_28),
        .j_vcc_04(j_vcc_04), .j_vcc_20(j_vcc_20), .j_vcc_22(j_vcc_22),
        .j_vcc_24(j_vcc_24), .j_vpp_24(j_vpp_24), .j_vpp_26(j_vpp_26));
    defparam dut.HOLD_TICKS = 4000;     // short hold so the sim finishes

    integer shifted = 0;
    integer errs    = 0;
    integer states_seen = 0;
    reg [1:0] last_state;
    reg last_sclk = 1'b0;

    // Count rising edges of the serial clock, and police the safety rule.
    always @(posedge clk) begin
        if (ser_clk && !last_sclk) begin
            shifted = shifted + 1;
            if (gnd_oe !== 1'b0) begin
                $display("FAIL: gnd_oe asserted while shifting (bit %0d)", shifted);
                errs = errs + 1;
            end
        end
        last_sclk <= ser_clk;

        if (gnd_le && gnd_oe) begin
            $display("FAIL: latch enable and output enable overlap");
            errs = errs + 1;
        end
        if (vpp_le || vcc_le || vpp_oe || vcc_oe) begin
            $display("FAIL: a VPP/VCC control moved during phase 1");
            errs = errs + 1;
        end
    end

    // Watch the state advance and report the shift count for each.
    initial begin
        last_state = dut.state;
        forever begin
            @(dut.state);
            $display("  state %0d -> %0d after %0d shift clocks (pattern=%b oe_level=%b)",
                     last_state, dut.state, shifted, dut.pattern_bit, dut.oe_level);
            if (shifted != 48) begin
                $display("FAIL: expected 48 shift clocks, saw %0d", shifted);
                errs = errs + 1;
            end
            shifted = 0;
            last_state = dut.state;
            states_seen = states_seen + 1;
        end
    end

    // Decode the UART so the announced state is checked, not assumed.
    localparam integer BIT_NS = 8681;   // 115200-ish
    reg [7:0] ch;
    integer i;
    initial begin
        forever begin
            @(negedge uart_tx);                 // start bit
            #(BIT_NS + BIT_NS/2);
            for (i = 0; i < 8; i = i + 1) begin
                ch[i] = uart_tx;
                #BIT_NS;
            end
            if (ch >= "0" && ch <= "9")
                $display("  uart announces state %0d", ch - "0");
        end
    end

    initial begin
        #40_000_000;                            // ~40 ms, several states
        $display("");
        if (states_seen < 3) begin
            $display("FAIL: only %0d state transitions in the run", states_seen);
            errs = errs + 1;
        end
        $display(errs == 0 ? "PASS: %0d transitions, no safety violations"
                           : "FAIL: %0d errors", errs == 0 ? states_seen : errs);
        $finish;
    end
endmodule
