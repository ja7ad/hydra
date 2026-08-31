# Hydra — build and packaging entry points.
#
#   make build      release build of CLI + GUI + IPC host (any OS)
#   make app        macOS .app bundle (target/release/Hydra Download Manager.app)
#   make dmg        macOS disk image                      -> target/dist/*.dmg
#   make deb        Debian/Ubuntu package                 -> target/dist/*.deb
#   make rpm        Fedora/RHEL/openSUSE package          -> target/dist/*.rpm
#   make linux      both deb and rpm
#   make appimage   portable, self-updating AppImage    -> target/dist/*.AppImage
#   make package    the right artifact(s) for the OS make runs on
#
#   make extensions  browser extensions: packed .xpi + .zip (+ .crx with a
#                    signing key), unpacked dirs and install instructions
#                    -> target/extensions
#
#   make ffi        the embeddable C library (static + shared) -> target/<profile>
#   make header     regenerate include/hydra.h from the Rust definitions
#   make header-check  fail if include/hydra.h is out of date
#   make ffi-compat the ABI 1 stability gate (docs/ffi/ABI.md)
#   make ffi-test   the FFI test suite plus the C ABI conformance program
#   make ffi-dist   a release archive of libhydra for the host target
#   make ffi-android  libhydra for the four Android ABIs (needs the NDK)
#   make ffi-apple  Hydra.xcframework: iOS device, simulator and macOS
#
# PROFILE=dist make build   selects the smaller panic=abort profile (Cargo.toml).

UNAME   := $(shell uname -s)
# The workspace product version ([workspace.package] in Cargo.toml), shared
# by the GUI, CLI and host bin crates that every package target bundles.
# The library crates version independently and don't affect artifact names.
VERSION := $(shell sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
PROFILE ?= release
CARGO   ?= cargo

.PHONY: all build cli gui host app dmg deb rpm linux appimage windows package clean \
        extensions \
        require-macos require-linux ffi header header-check ffi-compat \
        ffi-test ffi-dist ffi-android ffi-apple

all: build

build:
	$(CARGO) build --profile $(PROFILE) -p hya-cli -p hya-gui -p hya-host

cli:
	$(CARGO) build --profile $(PROFILE) -p hya-cli

gui:
	$(CARGO) build --profile $(PROFILE) -p hya-gui

host:
	$(CARGO) build --profile $(PROFILE) -p hya-host

# The browser extensions, in both shapes every packaging target ships: a packed
# .zip for the Chromium family and a packed .xpi for Firefox, next to the
# unpacked directories a developer-mode install loads. The package targets call
# the same script with --out pointing into their staging tree, so what a user
# installs is what this produces.
#
#   make extensions ARGS="--crx-key key.pem"   sign the Chromium .crx
#   make extensions ARGS=--sign                also fetch an AMO-signed .xpi
extensions:
	scripts/build-extensions.sh $(ARGS)

# The embeddable library. Builds every crate-type the manifest declares, so one
# invocation produces libhydra.a AND the shared library; the same include/hydra.h
# works against either.
#
# NOT built by `make build`: the CLI, GUI and IPC host are one product that ships
# together, and libhydra is a separate deliverable for third parties. Rolling it
# into the default target would slow every developer build for something most of
# them are not changing.
ffi:
	$(CARGO) build --profile $(PROFILE) -p hya-ffi
	@echo
	@echo "libhydra artifacts in target/$(PROFILE):"
	@ls -1 target/$(PROFILE)/libhydra.a target/$(PROFILE)/libhydra.so \
	      target/$(PROFILE)/libhydra.dylib target/$(PROFILE)/hydra.dll \
	      target/$(PROFILE)/hydra.lib 2>/dev/null || true
	@echo "header: include/hydra.h"

# include/hydra.h is generated and committed. Generated so it cannot drift from
# the implementation; committed so consuming libhydra needs no Rust toolchain.
header:
	scripts/gen-ffi-header.sh

header-check:
	scripts/gen-ffi-header.sh --check

# The other half of header-check. That one asks whether the header still matches
# the Rust; this one asks whether the change was ALLOWED -- every enumerator
# value, field offset, struct size and exported symbol in ABI 1 is frozen in
# crates/hydra-ffi/abi/abi-1.manifest, and additions the contract permits pass
# while movements do not. The rules are in docs/ffi/ABI.md.
ffi-compat:
	scripts/ffi-abi-compat.sh

# The Rust ABI suite, then the C conformance program. The second half is the one
# that catches a header that does not compile or a symbol that is not in the
# archive -- things no Rust test can see.
ffi-test:
	$(CARGO) test -p hya-ffi --all-targets
	scripts/ffi-abi-compat.sh
	scripts/ffi-c-example.sh

# Release archives. These call the SAME scripts the release workflow runs --
# an official artifact produced by steps that live only in a YAML file is one
# nobody outside CI can reproduce, and the point of libhydra is that somebody
# else compiles it into their own application.
#
#   make ffi-dist TARGET=aarch64-unknown-linux-musl   cross-compile
ffi-dist:
ifdef TARGET
	scripts/build-ffi.sh --target $(TARGET)
else
	scripts/build-ffi.sh
endif

ffi-android:
	scripts/package-ffi-android.sh

ffi-apple: require-macos
	scripts/package-ffi-apple.sh

# Ad-hoc-signed bundle with the GUI, CLI and hydra-host inside Contents/MacOS.
# Add ARGS=--install to also replace /Applications/Hydra Download Manager.app.
app: require-macos
	scripts/macos-app-bundle.sh $(ARGS)

dmg: require-macos
	scripts/package-macos-dmg.sh

deb: require-linux
	scripts/package-linux.sh deb

rpm: require-linux
	scripts/package-linux.sh rpm

linux: require-linux
	scripts/package-linux.sh all

# One self-contained file the user owns wherever they put it — which is also
# what lets Hydra update itself from it, unlike the deb and the rpm whose
# files belong to the package manager.
#
#   make appimage ARGS=--no-build   package binaries that are already built
appimage: require-linux
	scripts/package-appimage.sh $(ARGS)

windows:
	scripts/build-windows-installer.sh x64

package:
ifeq ($(UNAME),Darwin)
	$(MAKE) dmg
else
	$(MAKE) linux
endif

require-macos:
	@[ "$(UNAME)" = Darwin ] || { echo "error: this target must run on macOS (found $(UNAME))" >&2; exit 1; }

require-linux:
	@[ "$(UNAME)" = Linux ] || { echo "error: this target must run on Linux (found $(UNAME))" >&2; exit 1; }

clean:
	$(CARGO) clean
	rm -rf target/dist

fmt:
	cargo fmt --all

lint:
	cargo clippy --workspace --all-targets --all-features -- -D warnings

test:
	cargo test --workspace --all-features
