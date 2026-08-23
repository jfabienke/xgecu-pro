`timescale 1ns/1ps
`default_nettype none
module tb_beacon;
    localparam integer BIT_NS = 174*50;
    reg clk = 1'b0;
    always #25 clk = ~clk;

    wire j_02, j_05, zif_01, zif_48, j_gnd_27, j_gnd_28;

    t76_beacon dut (
        .i_clock_20M(clk), .j_02(j_02), .j_05(j_05),
        .zif_01(zif_01), .zif_48(zif_48),
        .j_gnd_27(j_gnd_27), .j_gnd_28(j_gnd_28)
    );

    reg [1:0] sel = 2'd0;
    wire line = (sel == 2'd0) ? j_02 :
                (sel == 2'd1) ? j_05 :
                (sel == 2'd2) ? zif_01 : zif_48;

    task decode;
        reg [7:0] ch;
        integer k, bi;
        begin
            for (k = 0; k < 8; k = k + 1) begin
                @(negedge line);                 // start bit
                #(BIT_NS + BIT_NS/2);            // middle of bit 0
                for (bi = 0; bi < 8; bi = bi + 1) begin
                    ch[bi] = line;
                    #(BIT_NS);
                end
                if (ch >= 8'h20 && ch < 8'h7F) $write("%c", ch);
                else $write("<%02X>", ch);
            end
            $write("\n");
        end
    endtask

    initial begin
        sel = 2'd0; $write("  j_02   -> "); decode;
        sel = 2'd1; $write("  j_05   -> "); decode;
        sel = 2'd2; $write("  zif_01 -> "); decode;
        sel = 2'd3; $write("  zif_48 -> "); decode;
        if (j_gnd_27 === 1'b1 && j_gnd_28 === 1'b1)
            $display("  ground reference pins 27/28 asserted");
        else
            $display("  FAIL: ground pins not asserted");
        $finish;
    end
    initial begin #60_000_000; $display("  TIMEOUT"); $finish; end
endmodule
`default_nettype wire
