# Edge mDNS example on Embassy

Simplest example I could do that connects to wifi from a Pi Pico W and then
makes a service discoverable with mDNS from edge-mdns

## Connect to your Wi-Fi network

Edit the SSID constants at the start of the source file to match your wifi network.

## Build a UF2 file for Pi Pico W
`make` will go a release cargo build and then build a UF2 firmware file from the ELF and leave it in:
```
target/thumbv6m-none-eabi/release/porky_mdns.uf2
```

Power the Pi Pico W (via USB plugin usually) while holding the BOOTSEL button and the bootloader will
wait for a firmware file to be downloaded

Download the file with your favorite utility. `cp` may work on Linux. On macos I had to use `ditto`

The device will start and connect to the Wi-Fi network and get an IP and then start the mDNS responder,
announcing the services with the names in the constants in the source file.

## Running with a debug probe

You should be able to run with a debug probe using probe-rs just using `cargo run --release`