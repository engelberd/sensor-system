# rpi-sanok deployment notes

This installation uses an eight-channel USB-to-RS485 converter handled by the
Linux `ch9344` kernel module. The converter driver is a host dependency and is
not vendored in this repository; install and maintain it on this host using the
vendor or operating-system package appropriate for the running kernel.

The stable system inventory, immutable hardware IDs, output rates and udev
aliases are in `rpi-sanok.json`. Lines E and F are omitted until their physical
nodes are installed and identified. Storage paths, temporary endpoint overrides
and temporarily disabled channels remain in the ignored
`host/system_config.json`.

After a kernel or driver update run:

```bash
./hostctl doctor
```
