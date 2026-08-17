# form — the only build entry point you need. See docs/specs/15-build-and-conventions.md.

CORE_DIR  := core
APP_DIR   := app
PROFILE   ?= debug
BUILD_DIR := $(APP_DIR)/build
APP_BUNDLE := $(BUILD_DIR)/form.app

ifeq ($(PROFILE),release)
  CARGO_FLAGS := --release
  SWIFT_FLAGS := -c release
else
  CARGO_FLAGS :=
  SWIFT_FLAGS :=
endif

CORE_LIB_DIR := $(abspath $(CORE_DIR)/target/$(PROFILE))
LINK_FLAGS   := -Xlinker -L$(CORE_LIB_DIR)

.PHONY: all debug release core app bundle run test test-rust test-swift lint fmt \
        headers check-symbols cli clean xcode verify-xcode

all: bundle

debug:
	@$(MAKE) bundle PROFILE=debug

release:
	@$(MAKE) bundle PROFILE=release

## Rust static library. Release is universal so the bundle runs on Intel too.
## Shared with the Xcode pre-build phase so the two paths cannot drift.
core:
	@echo "==> building rust core ($(PROFILE))"
	@bash scripts/build-core.sh $(PROFILE)

app: core
	@echo "==> swift build ($(PROFILE))"
	@cd $(APP_DIR) && swift build $(SWIFT_FLAGS) $(LINK_FLAGS)

bundle: app
	@bash scripts/build-app.sh $(PROFILE)

run: bundle
	@open $(APP_BUNDLE)

## The end-to-end proof with no Swift involved: streams a stub run to the terminal.
cli: core
	@cd $(CORE_DIR) && cargo run $(CARGO_FLAGS) --bin form-cli -- chat "Add a health check endpoint"

test: test-rust test-swift

test-rust:
	@cd $(CORE_DIR) && cargo test

test-swift: core
	@cd $(APP_DIR) && swift test $(LINK_FLAGS)

lint:
	@cd $(CORE_DIR) && cargo fmt --all -- --check
	@cd $(CORE_DIR) && cargo clippy --all-targets -- -D warnings

fmt:
	@cd $(CORE_DIR) && cargo fmt --all

## Regenerate core/include/form.h. The generator is a separate out-of-workspace crate so
## cbindgen is not a dependency of every build; `cd` matters, its .cargo/config.toml
## redirects the target dir into the gitignored core/target/.
headers:
	@cd $(CORE_DIR)/crates/form-ffi/tools/headergen && cargo run --quiet
	@echo "    core/include/form.h regenerated"

## The header promises these symbols; this is what catches a rename before Swift does.
check-symbols: core
	@echo "==> checking exported C symbols"
	@for sym in form_abi_version form_core_new form_core_free form_core_subscribe \
	            form_core_unsubscribe form_core_query form_core_dispatch \
	            form_string_free form_last_error; do \
		nm -gU $(CORE_LIB_DIR)/libform_ffi.a 2>/dev/null | grep -q "_$$sym$$" \
			|| { echo "missing exported symbol: $$sym"; exit 1; }; \
	done
	@echo "    all 9 present"

## Build through Xcode exactly as pressing Run does, then assert the Rust core actually
## made it into the product. Passes once the app constructs a CoreClient at startup (W9);
## until then the app never references the core and the linker drops it.
verify-xcode: form.xcodeproj
	@xcodebuild -project form.xcodeproj -scheme form -configuration Debug build \
		-quiet -derivedDataPath $(BUILD_DIR)/DerivedData
	@find $(BUILD_DIR)/DerivedData/Build/Products/Debug -type f \
		\( -name "form" -o -name "*.dylib" \) -exec strings {} \; 2>/dev/null \
		| grep -q "already-freed core handle" \
		&& echo "    rust core is linked into the Xcode product" \
		|| { echo "the Xcode build did not link the Rust core"; exit 1; }

## Generate form.xcodeproj and open it. The project is not committed — it is regenerated
## from project.yml, so it cannot drift or conflict. Hitting Run in Xcode builds the Rust
## core first via a pre-build phase.
xcode: form.xcodeproj
	@open form.xcodeproj

form.xcodeproj: project.yml app/Package.swift
	@command -v xcodegen >/dev/null || { \
		echo "xcodegen not found. Install it with: brew install xcodegen"; exit 1; }
	@echo "==> xcodegen"
	@xcodegen generate --quiet

clean:
	@cd $(CORE_DIR) && cargo clean
	@rm -rf $(APP_DIR)/.build $(BUILD_DIR)
