import_device eagle_20.db -package BGA256X
read_verilog t76_baudsweep.v
read_adc t76_baudsweep.adc
read_sdc t76_baudsweep.sdc
optimize_rtl
optimize_gate
legalize_phy_inst
place
route
bitgen -bit t76_baudsweep.bit -version 0X00 -g ucode:000000000000000000000000 -info -log_file t76_baudsweep_bit.log
exit
