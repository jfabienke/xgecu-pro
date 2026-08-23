    input  wire i_clock_20M,
    output wire uart_tx,
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
    output wire j_vpp_26,

    // Observed pins: INPUT ONLY. Never declared inout, so the
    // toolchain cannot infer a driver and contend with the MCU.
    input  wire HD0           ,  // L8    Cpu: 15
    input  wire HD1           ,  // P5    Cpu: 17
    input  wire HD10          ,  // C5    Cpu: 26
    input  wire HD11          ,  // D6    Cpu: 27
    input  wire HD12          ,  // C6    Cpu: 28
    input  wire HD13          ,  // E6    Cpu: 29
    input  wire HD14          ,  // E7    Cpu: 30
    input  wire HD15          ,  // A7    Cpu: 31
    input  wire HD16          ,  // P13   Cpu: 32
    input  wire HD18          ,  // R3    Cpu: 34
    input  wire HD19          ,  // R11   Cpu: 35
    input  wire HD2           ,  // C10   Cpu: 53
    input  wire HD20          ,  // C7    Cpu: 36
    input  wire HD21          ,  // A4    Cpu: 37
    input  wire HD22          ,  // B5    Cpu: 38
    input  wire HD3           ,  // N5    Cpu: 19
    input  wire HD31          ,  // T10   Cpu: 51
    input  wire HD4           ,  // P12   Cpu: 20
    input  wire HD5           ,  // N12   Cpu: 21
    input  wire HD6           ,  // P10   Cpu: 22
    input  wire HD7           ,  // M9    Cpu:23
    input  wire HD8           ,  // D8    Cpu: 24
    input  wire HD9           ,  // D5    Cpu: 25
    input  wire HRACT         ,  // T5    Cpu: 11
    input  wire HRCLK         ,  // R7    Cpu: 10
    input  wire HRVLD         ,  // L7    Cpu: 14
    input  wire HTACK         ,  // C8    Cpu: 55
    input  wire HTCLK         ,  // E8    Cpu: 56
    input  wire HTRDY         ,  // T4    Cpu: 18
    input  wire HTREQ         ,  // C9    Cpu: 54
    input  wire HTVLD         ,  // C11   Cpu: 52
    input  wire cpu_p13       ,  // R5    Cpu: 13
    input  wire m0            ,  // T11   M0
    input  wire m1            ,  // N11   M1
    input  wire unknown_c4    ,  // C4    ? ro5 ?
    input  wire zif_01        ,  // B15   ZIF01
    input  wire zif_02        ,  // B16   ZIF02
    input  wire zif_03        ,  // C15   ZIF03
    input  wire zif_04        ,  // C16   ZIF04
    input  wire zif_05        ,  // D16   ZIF05
    input  wire zif_06        ,  // E16   ZIF06
    input  wire zif_07        ,  // F15   ZIF07
    input  wire zif_08        ,  // F16   ZIF08
    input  wire zif_09        ,  // G14   ZIF09
    input  wire zif_10        ,  // G16   ZIF10
    input  wire zif_11        ,  // H15   ZIF11
    input  wire zif_12        ,  // H16   ZIF12
    input  wire zif_13        ,  // F2    ZIF13
    input  wire zif_14        ,  // G1    ZIF14
    input  wire zif_15        ,  // F1    ZIF15
    input  wire zif_16        ,  // E2    ZIF16
    input  wire zif_17        ,  // E1    ZIF17
    input  wire zif_18        ,  // D1    ZIF18
    input  wire zif_19        ,  // C1    ZIF19
    input  wire zif_20        ,  // B1    ZIF20
    input  wire zif_21        ,  // B2    ZIF21
    input  wire zif_22        ,  // A2    ZIF22
    input  wire zif_23        ,  // B3    ZIF23
    input  wire zif_24        ,  // A3    ZIF24
    input  wire zif_25        ,  // R2    ZIF25
    input  wire zif_26        ,  // R1    ZIF26
    input  wire zif_27        ,  // P2    ZIF27
    input  wire zif_28        ,  // P1    ZIF28
    input  wire zif_29        ,  // N3    ZIF29
    input  wire zif_30        ,  // N1    ZIF30
    input  wire zif_31        ,  // M1    ZIF31
    input  wire zif_32        ,  // M2    ZIF32
    input  wire zif_33        ,  // K2    ZIF33
    input  wire zif_34        ,  // H1    ZIF34
    input  wire zif_35        ,  // H2    ZIF35
    input  wire zif_36        ,  // J1    ZIF36
    input  wire zif_37        ,  // J14   ZIF37
    input  wire zif_38        ,  // J16   ZIF38
    input  wire zif_39        ,  // K16   ZIF39
    input  wire zif_40        ,  // N14   ZIF40
    input  wire zif_41        ,  // N16   ZIF41
    input  wire zif_42        ,  // P15   ZIF42
    input  wire zif_43        ,  // R16   ZIF43
    input  wire zif_44        ,  // T15   ZIF44
    input  wire zif_45        ,  // R14   ZIF45
    input  wire zif_46        ,  // T14   ZIF46
    input  wire zif_47        ,  // R12   ZIF47
    input  wire zif_48        ,  // T12   ZIF48
    input  wire j_02          ,  // H3    ISP:J02
    input  wire j_03          ,  // H4    ISP:J03
    input  wire j_04          ,  // J3    ISP:J04
    input  wire j_05          ,  // L3    ISP:J05
    input  wire j_06          ,  // M3    ISP:J06
    input  wire j_07          ,  // M5    ISP:J07
    input  wire j_09          ,  // M14   ISP:J09
    input  wire j_10          ,  // J11   ISP:J10
    input  wire j_12          ,  // K14   ISP:J12
    input  wire j_13          ,  // H14   ISP:J13
    input  wire j_14          ,  // H13   ISP:J14
    input  wire j_16          ,  // F12   ISP:J16
    input  wire j_17          ,  // E15   ISP:J17
    input  wire j_18          ,  // D14   ISP:J18
    input  wire j_19          ,  // E13   ISP:J19
    input  wire j_20          ,  // E12   ISP:J20
    input  wire j_22          ,  // C3    ISP:J22
    input  wire j_23          ,  // C2    ISP:J23
    input  wire j_24          ,  // D3    ISP:J24
    input  wire j_25            // E3    ISP:J25
