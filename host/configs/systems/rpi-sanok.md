# rpi-sanok deployment notes

This installation uses an eight-channel USB-to-RS485 converter handled by the
Linux `ch9344` kernel module. The converter driver is a host dependency and is
not vendored in this repository; install and maintain it on this host using the
vendor or operating-system package appropriate for the running kernel.

The stable system inventory and sensor settings are in `rpi-sanok.json`.
Concrete `/dev/ttyCH9344USB*` paths, storage paths and temporarily disabled
channels remain in the ignored `host/system_config.json`.

After a kernel or driver update run:

```bash
./hostctl doctor
```
