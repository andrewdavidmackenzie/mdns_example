all:
	cargo build --release
	elf2uf2-rs target/thumbv6m-none-eabi/release/porky_mdns target/thumbv6m-none-eabi/release/porky_mdns.uf2