import_device eagle_20.db -package BGA256X
read_verilog t76_hspi_probe.v
read_adc t76_hspi_probe.adc
read_sdc t76_hspi_probe.sdc
optimize_rtl
optimize_gate
legalize_phy_inst
place
route
bitgen -bit t76_hspi_probe.bit -version 0X00 -g ucode:000000000000000000000000 -info -log_file t76_hspi_probe_bit.log
exit
