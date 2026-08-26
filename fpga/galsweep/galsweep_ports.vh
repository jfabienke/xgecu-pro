    input  wire i_clock_20M,
    input  wire uart_rx,
    output wire uart_tx,
    output wire drv_01      ,  // F1   ZIF15  chip pin 1
    output wire drv_02      ,  // E2   ZIF16  chip pin 2
    output wire drv_03      ,  // E1   ZIF17  chip pin 3
    output wire drv_04      ,  // D1   ZIF18  chip pin 4
    output wire drv_05      ,  // C1   ZIF19  chip pin 5
    output wire drv_06      ,  // B1   ZIF20  chip pin 6
    output wire drv_07      ,  // B2   ZIF21  chip pin 7
    output wire drv_08      ,  // A2   ZIF22  chip pin 8
    output wire drv_09      ,  // B3   ZIF23  chip pin 9
    output wire drv_11      ,  // R2   ZIF25  chip pin 11
    input  wire smp_12      ,  // R1   ZIF26  chip pin 12
    input  wire smp_13      ,  // P2   ZIF27  chip pin 13
    input  wire smp_14      ,  // P1   ZIF28  chip pin 14
    input  wire smp_15      ,  // N3   ZIF29  chip pin 15
    input  wire smp_16      ,  // N1   ZIF30  chip pin 16
    input  wire smp_17      ,  // M1   ZIF31  chip pin 17
    input  wire smp_18      ,  // M2   ZIF32  chip pin 18
    input  wire smp_19      ,  // K2   ZIF33  chip pin 19
    output wire ser_clk,
    output wire ser_data,
    output wire vpp_le,
    output wire vcc_le,
    output wire gnd_le,
    output wire vpp_oe,
    output wire vcc_oe,
    output wire gnd_oe,
    output wire j_gnd_11,
    output wire j_gnd_21,
    output wire j_gnd_26,
    output wire j_gnd_27,
    output wire j_gnd_28,
    output wire j_vcc_04,
    output wire j_vcc_20,
    output wire j_vcc_22,
    output wire j_vcc_24,
    output wire j_vpp_24,
    output wire j_vpp_26
