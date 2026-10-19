# Build the rust-ime fcitx5 addon.
#
# Requires: libfcitx5core-dev (headers + fcitx5.pc), g++ (C++20).
# The Rust core is built as a staticlib and linked into the shared object.

CXX      ?= g++
CXXFLAGS ?= -O2 -fPIC -std=c++20 -Wall -Wextra
PKG       = Fcitx5Core

FCITX_CFLAGS := $(shell pkg-config --cflags $(PKG) 2>/dev/null)
FCITX_LIBS   := $(shell pkg-config --libs $(PKG) 2>/dev/null)

RUST_ROOT   = ../..
RUST_LIB    = $(RUST_ROOT)/target/release/libime_engine.a
OUT         = librustime.so

all: $(OUT)

$(RUST_LIB):
	cd $(RUST_ROOT) && cargo build --release -p ime-engine

$(OUT): src/rustime.cpp $(RUST_LIB)
	@if [ -z "$(FCITX_CFLAGS)" ]; then \
		echo "error: Fcitx5Core not found. Install libfcitx5core-dev."; \
		exit 1; \
	fi
	$(CXX) $(CXXFLAGS) $(FCITX_CFLAGS) -shared -o $@ src/rustime.cpp \
		$(RUST_LIB) $(FCITX_LIBS) -lpthread -ldl -lm

clean:
	rm -f $(OUT)

.PHONY: all clean
