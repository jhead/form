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
        headers check-symbols cli clean xcode

all: bundle

debug:
	@$(MAKE) bundle PROFILE=debug

release:
	@$(MAKE) bundle PROFILE=release

## Rust static library. Release is universal so the bundle runs on Intel too.
core:
	@echo "==> cargo build ($(PROFILE))"
	@cd $(CORE_DIR) && cargo build $(CARGO_FLAGS)

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

## TODO(W6): generate with cbindgen and add the drift test.
headers:
	@echo "core/include/form.h is currently hand-maintained — see docs/specs/06-ffi.md §1"

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

## Optional convenience only — SwiftPM is the source of truth, no .xcodeproj is committed.
xcode:
	@cd $(APP_DIR) && swift package generate-xcodeproj 2>/dev/null \
		|| echo "open the package directly: xed $(APP_DIR)"

clean:
	@cd $(CORE_DIR) && cargo clean
	@rm -rf $(APP_DIR)/.build $(BUILD_DIR)
