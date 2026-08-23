import_device eagle_20.db -package BGA256X
read_verilog t76_census.v
read_adc t76_census.adc
read_sdc t76_census.sdc
optimize_rtl
optimize_gate
legalize_phy_inst
place
route
report_area
report_timing
write_pnl t76_census.pnl
bitgen -bit t76_census.bit -version 0X00 -g ucode:000000000000000000000000 -info -log_file t76_census_bit.log
exit
