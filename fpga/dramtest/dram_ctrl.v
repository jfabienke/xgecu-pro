// ---------------------------------------------------------------------------
// Asynchronous DRAM controller and march tester for the T76's EG4X20 FPGA.
//
// Targets the single-rail +5 V generation: 4164 (64K x 1), 41256 (256K x 1),
// 4464 (64K x 4), 44256 (256K x 4) and relatives. Earlier three-rail parts --
// the 4116 above all -- are permanently out of reach because the T76's voltage
// field is a VPP/VCC code with no negative-rail encoding, so no bitstream can
// produce the -5 V they need.
//
// Everything here is in clock cycles rather than nanoseconds, so the same
// design works from CLK_20 (50 ns) or from HTCLK (~12.5 ns) without editing
// timing by hand. Defaults below are cycles at 20 MHz for a 150 ns part, with
// a cycle of margin on each edge.
//
// Refresh is distributed, not burst: the row counter advances on its own
// interval and preempts the test engine between accesses. A DRAM under test is
// only meaningfully under test if it is being refreshed correctly, and a
// tester that loses rows to neglect reports faults that are its own.
// ---------------------------------------------------------------------------

`default_nettype none

module dram_ctrl #(
    parameter integer ROW_BITS   = 8,     // 4164: 8 rows bits, 41256: 9
    parameter integer COL_BITS   = 8,
    parameter integer DQ_BITS    = 1,     // 1 for x1 parts, 4 for x4

    // --- timings, in clock cycles (defaults: 20 MHz, 150 ns DRAM) ----------
    parameter integer T_RP       = 3,     // RAS precharge      >= 100 ns
    parameter integer T_RAS      = 4,     // RAS pulse width    >= 150 ns
    parameter integer T_RCD      = 2,     // RAS to CAS delay   >=  50 ns
    parameter integer T_CAS      = 2,     // CAS pulse width    >=  75 ns
    parameter integer T_ASR      = 1,     // row address setup
    parameter integer T_REFI     = 300,   // refresh interval, ~15 us at 20 MHz
    parameter integer INIT_CYCLES= 8      // wake-up refreshes the datasheet asks for
) (
    input  wire clk,
    input  wire start,                    // pulse to begin a pass

    // --- DRAM pins ---------------------------------------------------------
    output reg  ras_n,
    output reg  cas_n,
    output reg  we_n,
    output reg  [ROW_BITS-1:0] addr,      // multiplexed row/column
    output reg  [DQ_BITS-1:0]  dq_out,
    output reg                 dq_drive,  // 1 while we own the data lines
    input  wire [DQ_BITS-1:0]  dq_in,

    // --- results -----------------------------------------------------------
    output reg  busy,
    output reg  done,
    output reg  [31:0] errors,            // saturating count of mismatches
    output reg  [ROW_BITS+COL_BITS-1:0] first_bad,
    output reg  first_bad_valid,
    output reg  [3:0] phase                // which march element is running
);

localparam integer ADDR_BITS = ROW_BITS + COL_BITS;

// --- refresh -----------------------------------------------------------
// RAS-only refresh: put the row on the address lines, pulse RAS with CAS
// held high. The row counter free-runs so every row is visited in turn.
reg [15:0] ref_timer = 16'd0;
reg [ROW_BITS-1:0] ref_row = {ROW_BITS{1'b0}};
reg ref_pending = 1'b0;
reg ref_ack     = 1'b0;   // declared here: the refresh block above consumes it
// A refresh shares the precharge state with a real access, so without this
// flag it raises acc_done too -- the march engine then checks stale read data
// and advances past an address it never accessed. Refresh must be invisible
// to the engine it protects.
reg in_refresh  = 1'b0;

always @(posedge clk) begin
    if (ref_timer >= T_REFI[15:0]) begin
        ref_timer   <= 16'd0;
        ref_pending <= 1'b1;
    end else begin
        ref_timer <= ref_timer + 16'd1;
    end
    if (ref_ack) begin
        ref_pending <= 1'b0;
        ref_row     <= ref_row + {{ROW_BITS-1{1'b0}}, 1'b1};
    end
end

// --- sequencer ---------------------------------------------------------
localparam [3:0]
    S_IDLE    = 4'd0,
    S_INIT    = 4'd1,   // the wake-up refresh burst
    S_W0      = 4'd2,   // write 0 everywhere
    S_R0W1    = 4'd3,   // read 0, write 1, ascending
    S_R1W0    = 4'd4,   // read 1, write 0, descending
    S_R0      = 4'd5,   // final read of 0
    S_DONE    = 4'd6;

localparam [2:0]
    A_IDLE = 3'd0, A_ROW = 3'd1, A_RCD = 3'd2,
    A_COL  = 3'd3, A_CAS = 3'd4, A_PRE = 3'd5, A_REF = 3'd6;

reg [3:0] st = S_IDLE;
reg [2:0] acc = A_IDLE;
reg [15:0] tmr = 16'd0;
reg [ADDR_BITS-1:0] a = {ADDR_BITS{1'b0}};
reg descending = 1'b0;
reg do_read = 1'b0, do_write = 1'b0;
reg [DQ_BITS-1:0] wr_data = {DQ_BITS{1'b0}};
reg [DQ_BITS-1:0] expect  = {DQ_BITS{1'b0}};
// Request/acknowledge, not a pulse. acc_start used to be a single-cycle pulse,
// and a refresh taking priority in A_IDLE that cycle swallowed it -- the access
// never ran and the march engine waited forever. The spurious acc_done that
// refresh also raised was masking exactly this, so fixing one exposed the other.
reg acc_req   = 1'b0;    // march engine sets, access engine clears
reg acc_taken = 1'b0;
reg acc_done  = 1'b0;
reg [DQ_BITS-1:0] rd_data = {DQ_BITS{1'b0}};

wire [ROW_BITS-1:0] row = a[ADDR_BITS-1 -: ROW_BITS];
wire [COL_BITS-1:0] col = a[COL_BITS-1:0];
wire last_addr = descending ? (a == {ADDR_BITS{1'b0}})
                            : (a == {ADDR_BITS{1'b1}});

// --- access engine: one read, write or refresh cycle -------------------
always @(posedge clk) begin
    ref_ack   <= 1'b0;
    acc_done  <= 1'b0;
    acc_taken <= 1'b0;

    case (acc)
    A_IDLE: begin
        ras_n <= 1'b1; cas_n <= 1'b1; we_n <= 1'b1; dq_drive <= 1'b0;
        // Refresh outranks the test engine, but only between accesses -- never
        // inside one, which would corrupt the very cycle being measured.
        if (ref_pending) begin
            addr       <= ref_row;
            tmr        <= 16'd0;
            in_refresh <= 1'b1;
            acc        <= A_REF;
        end else if (acc_req) begin
            addr       <= row;
            tmr        <= 16'd0;
            in_refresh <= 1'b0;
            acc_taken  <= 1'b1;
            acc        <= A_ROW;
        end
    end

    A_REF: begin                      // RAS-only refresh
        ras_n <= 1'b0;
        if (tmr >= T_RAS[15:0]) begin
            ras_n <= 1'b1; tmr <= 16'd0; acc <= A_PRE;
        end else tmr <= tmr + 16'd1;
        if (tmr == 16'd0) ref_ack <= 1'b1;
    end

    A_ROW: begin                      // row address, then RAS low
        if (tmr >= T_ASR[15:0]) begin
            ras_n <= 1'b0; tmr <= 16'd0; acc <= A_RCD;
        end else tmr <= tmr + 16'd1;
    end

    A_RCD: begin
        if (tmr >= T_RCD[15:0]) begin
            // plain assignment: Verilog zero-extends a narrower RHS, and the
            // explicit replication is illegal when ROW_BITS == COL_BITS
            addr <= col;
            // Early write: WE and data are presented before CAS falls, so DOUT
            // stays high-Z and a x1 part's separate DIN/DOUT never contend.
            // A read-modify element sets BOTH do_read and do_write -- read half
            // first, then write half. One access does one thing, so WE is
            // asserted only on the write half. Asserting it whenever do_write
            // was set made every access a write, and the read half then
            // "passed" purely because the expected value happened to be zero.
            if (do_write && !do_read) begin
                we_n     <= 1'b0;
                dq_out   <= wr_data;
                dq_drive <= 1'b1;
            end
            tmr <= 16'd0; acc <= A_COL;
        end else tmr <= tmr + 16'd1;
    end

    A_COL: begin
        cas_n <= 1'b0;
        tmr   <= 16'd0;
        acc   <= A_CAS;
    end

    A_CAS: begin
        if (tmr >= T_CAS[15:0]) begin
            if (do_read) rd_data <= dq_in;   // sampled at the end of CAS low
            cas_n    <= 1'b1;
            ras_n    <= 1'b1;
            we_n     <= 1'b1;
            dq_drive <= 1'b0;
            tmr      <= 16'd0;
            acc      <= A_PRE;
        end else tmr <= tmr + 16'd1;
    end

    A_PRE: begin                      // RAS precharge before anything else
        if (tmr >= T_RP[15:0]) begin
            tmr <= 16'd0;
            acc <= A_IDLE;
            acc_done <= ~in_refresh;      // a refresh completes nothing
        end else tmr <= tmr + 16'd1;
    end

    default: acc <= A_IDLE;
    endcase
end

// --- march engine ------------------------------------------------------
// March C- reduced to the elements that catch the faults a socket tester can
// actually act on: stuck-at, transition, and coupling between cells.
reg [15:0] init_n = 16'd0;

always @(posedge clk) begin
    if (acc_taken) acc_req <= 1'b0;

    case (st)
    S_IDLE: begin
        busy <= 1'b0;
        if (start) begin
            busy <= 1'b1; done <= 1'b0; errors <= 32'd0;
            first_bad_valid <= 1'b0; init_n <= 16'd0;
            st <= S_INIT; phase <= 4'd0;
        end
    end

    S_INIT: begin
        // The datasheets ask for a pause and a number of refresh cycles before
        // the array is trustworthy. The refresh engine is already running, so
        // this just counts them off.
        if (ref_ack) init_n <= init_n + 16'd1;
        if (init_n >= INIT_CYCLES[15:0]) begin
            a <= {ADDR_BITS{1'b0}}; descending <= 1'b0;
            do_read <= 1'b0; do_write <= 1'b1; wr_data <= {DQ_BITS{1'b0}};
            acc_req <= 1'b1; st <= S_W0; phase <= 4'd1;
        end
    end

    S_W0, S_R0W1, S_R1W0, S_R0: begin
        if (acc_done) begin
            // check the read half of a read-modify element
            if (do_read && rd_data !== expect) begin
                if (errors != 32'hFFFFFFFF) errors <= errors + 32'd1;
                if (!first_bad_valid) begin
                    first_bad <= a; first_bad_valid <= 1'b1;
                end
            end
            if (do_read && do_write) begin
                // second half of this address: write the new value
                do_read <= 1'b0;
                acc_req <= 1'b1;
            end else begin
                if (last_addr) begin
                    case (st)
                    S_W0:   begin a <= {ADDR_BITS{1'b0}}; descending <= 1'b0;
                                  do_read <= 1'b1; do_write <= 1'b1;
                                  expect <= {DQ_BITS{1'b0}}; wr_data <= {DQ_BITS{1'b1}};
                                  st <= S_R0W1; phase <= 4'd2; acc_req <= 1'b1; end
                    S_R0W1: begin a <= {ADDR_BITS{1'b1}}; descending <= 1'b1;
                                  do_read <= 1'b1; do_write <= 1'b1;
                                  expect <= {DQ_BITS{1'b1}}; wr_data <= {DQ_BITS{1'b0}};
                                  st <= S_R1W0; phase <= 4'd3; acc_req <= 1'b1; end
                    S_R1W0: begin a <= {ADDR_BITS{1'b0}}; descending <= 1'b0;
                                  do_read <= 1'b1; do_write <= 1'b0;
                                  expect <= {DQ_BITS{1'b0}};
                                  st <= S_R0; phase <= 4'd4; acc_req <= 1'b1; end
                    default:begin st <= S_DONE; phase <= 4'd5; end
                    endcase
                end else begin
                    a <= descending ? a - {{ADDR_BITS-1{1'b0}},1'b1}
                                    : a + {{ADDR_BITS-1{1'b0}},1'b1};
                    if (st != S_R0) do_read <= (st != S_W0);
                    acc_req <= 1'b1;
                end
            end
        end
    end

    S_DONE: begin
        busy <= 1'b0; done <= 1'b1;
        if (!start) st <= S_IDLE;
    end

    default: st <= S_IDLE;
    endcase
end

initial begin
    ras_n = 1'b1; cas_n = 1'b1; we_n = 1'b1; dq_drive = 1'b0;
    addr = {ROW_BITS{1'b0}}; dq_out = {DQ_BITS{1'b0}};
    busy = 1'b0; done = 1'b0; errors = 32'd0;
    first_bad = {ADDR_BITS{1'b0}}; first_bad_valid = 1'b0; phase = 4'd0;
end

endmodule

`default_nettype wire
