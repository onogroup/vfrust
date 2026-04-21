.PHONY: build build-release sign run clean test test-unit test-e2e

build:
	cargo build

build-release:
	cargo build --release

sign: build
	codesign --entitlements vfrust.entitlements -s - target/debug/vfrust-cli

sign-release: build-release
	codesign --entitlements vfrust.entitlements -s - target/release/vfrust-cli

run: sign
	target/debug/vfrust-cli $(ARGS)

test-unit:
	cargo test --lib --all
	cargo test --test config

test-e2e:
	cargo test --tests --no-run
	@for bin in target/debug/deps/vm_lifecycle_efi-* target/debug/deps/vm_lifecycle_linux-* \
	            target/debug/deps/network-* target/debug/deps/cloudinit_e2e-* \
	            target/debug/deps/serial-* target/debug/deps/devices-* \
	            target/debug/deps/storage-* target/debug/deps/fs_share-* \
	            target/debug/deps/snapshot-* target/debug/deps/metrics-*; do \
	    if [ -f "$$bin" ] && [ -x "$$bin" ]; then \
	        codesign --force --entitlements vfrust.entitlements -s - "$$bin" 2>/dev/null || true; \
	    fi; \
	done
	cargo test --tests -- --test-threads=1 $(ARGS)

test: test-unit test-e2e

clean:
	cargo clean
