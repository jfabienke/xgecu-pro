import_device eagle_20.db -package BGA256X
read_verilog t76_railsweep.v
read_adc t76_railsweep.adc
read_sdc t76_railsweep.sdc
optimize_rtl
optimize_gate
legalize_phy_inst
place
route
bitgen -bit t76_railsweep.bit -version 0X00 -g ucode:000000000000000000000000 -info -log_file t76_railsweep_bit.log
exit
