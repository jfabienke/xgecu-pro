import_device eagle_20.db -package BGA256X
read_verilog t76_galsweep.v
read_adc t76_galsweep.adc
read_sdc t76_galsweep.sdc
optimize_rtl
optimize_gate
legalize_phy_inst
place
route
bitgen -bit t76_galsweep.bit -version 0X00 -g ucode:000000000000000000000000 -info -log_file t76_galsweep_bit.log
exit
