// Verifies the DRAM controller against a behavioural async-DRAM model.
//
// Two properties matter and both are checked: a good part must pass with zero
// errors, and a faulty part must be CAUGHT. A tester that never reports a fault
// is worthless, and a green run against a perfect model proves only half of it.
`timescale 1ns/1ps
`default_nettype none

module tb_dram;
    localparam integer RB = 4, CB = 4;          // 256 cells: enough to march, fast to simulate
    localparam integer N  = (1<<RB)*(1<<CB);

    reg clk = 1'b0;
    always #25 clk = ~clk;                      // 20 MHz

    reg start = 1'b0;
    wire ras_n, cas_n, we_n, dq_drive;
    wire [RB-1:0] addr;
    wire dq_out;
    wire busy, done, first_bad_valid;
    wire [31:0] errors;
    wire [RB+CB-1:0] first_bad;
    wire [3:0] phase;

    // fault injection: force one cell to a stuck value
    reg        inject   = 1'b0;
    reg [RB+CB-1:0] bad_cell = 8'h5A;
    reg        bad_val  = 1'b1;

    // --- behavioural async DRAM, x1 -------------------------------------
    reg mem [0:N-1];
    reg [RB-1:0] row_l;
    reg [CB-1:0] col_l;
    reg dout_r = 1'b0;
    integer rdtrace = 0;
    integer i;
    initial begin
        for (i = 0; i < N; i = i + 1) mem[i] = 1'b0;
    end

    wire dq_in = dout_r;

    always @(negedge ras_n) row_l <= addr;      // row latched on RAS falling

    always @(negedge cas_n) begin
        if (ras_n === 1'b0) begin               // a real access, not refresh
            col_l = addr[CB-1:0];
            #1;
            if (we_n === 1'b0) begin            // early write
                mem[{row_l, col_l}] = dq_out;
            end else begin
                dout_r <= (inject && {row_l,col_l} == bad_cell)
                          ? bad_val : mem[{row_l, col_l}];
            end
        end
    end

    dram_ctrl #(.ROW_BITS(RB), .COL_BITS(CB), .DQ_BITS(1),
                .T_REFI(60), .INIT_CYCLES(4)) dut (
        .clk(clk), .start(start),
        .ras_n(ras_n), .cas_n(cas_n), .we_n(we_n), .addr(addr),
        .dq_out(dq_out), .dq_drive(dq_drive), .dq_in(dq_in),
        .busy(busy), .done(done), .errors(errors),
        .first_bad(first_bad), .first_bad_valid(first_bad_valid), .phase(phase)
    );

    integer fails = 0;
    task expect(input cond, input [255:0] what);
        begin if (!cond) begin $display("  FAIL: %0s", what); fails = fails + 1; end end
    endtask

    task run_pass;
        begin
            start = 1'b1; @(posedge clk); @(posedge clk); start = 1'b0;
            wait (done === 1'b1);
            @(posedge clk);
        end
    endtask

    initial begin
        #500;

        // --- 1. a good part must pass ------------------------------------
        inject = 1'b0;
        run_pass;
        $display("  good part   : errors=%0d  phase=%0d", errors, phase);
        expect(errors == 32'd0, "a good part passes with zero errors");
        expect(done === 1'b1,   "the pass completes");

        #2000;

        // --- 2. a stuck cell must be caught ------------------------------
        inject = 1'b1; bad_cell = 8'h5A; bad_val = 1'b1;
        run_pass;
        $display("  stuck-at-1  : errors=%0d  first_bad=0x%02X (injected 0x%02X)",
                 errors, first_bad, bad_cell);
        expect(errors != 32'd0,            "a stuck cell is detected");
        expect(first_bad_valid === 1'b1,   "the failing address is reported");
        expect(first_bad == bad_cell,      "the reported address is the injected one");

        if (fails == 0) $display("  dram_ctrl: all checks passed");
        else            $display("  %0d FAILURES", fails);
        $finish;
    end

    initial begin #200_000_000; $display("  TIMEOUT"); $finish; end
endmodule

`default_nettype wire
