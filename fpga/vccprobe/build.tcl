import_device eagle_20.db -package BGA256X
read_verilog t76_vccprobe.v
read_adc t76_vccprobe.adc
read_sdc t76_vccprobe.sdc
optimize_rtl
optimize_gate
legalize_phy_inst
place
route
bitgen -bit t76_vccprobe.bit -version 0X00 -g ucode:000000000000000000000000 -info -log_file t76_vccprobe_bit.log
exit
