.PHONY: gui daemon build install check

# Start the GUI in dev mode (hot-reload for JS/CSS, rebuilds Rust on change).
# The daemon must already be running (make daemon).
# sg curb ensures the process has the curb group even before a logout/login.
gui:
	cd gui && sg curb -c "CURB_SOCK=/run/curbd/curbd.sock ./node_modules/.bin/tauri dev"

# Build and (re)start the daemon in the foreground.
# Run in a separate terminal; Ctrl-C to stop.
daemon:
	cargo build -p curbd
	sudo -E ./target/debug/curbd

# Build everything (release).
build:
	cargo build --release -p curbd -p curb
	cd gui && cargo tauri build

# Install system-wide (creates curb group, enables systemd service).
install:
	sudo packaging/install.sh

# Quick sanity check — ping the running daemon.
check:
	./target/debug/curb ping
