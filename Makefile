.PHONY: all build release test self-test preview appimage verify install clean

all: release

build:
	cargo build

release:
	cargo build --release

test:
	cargo test

self-test: release
	./target/release/thetalk --self-test

preview: release
	./target/release/thetalk --render-preview assets/screenshots

appimage:
	./scripts/build-appimage.sh

verify:
	./scripts/verify_on_niri.sh

install:
	./scripts/install-local.sh

clean:
	cargo clean
