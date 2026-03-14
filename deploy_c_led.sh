#!/bin/bash
set -e

APP="led"
WORKSPACE_DIR="/home/rustuser/projects/rust/LegoSpikeRust"
SPIKE_RT_DIR="$WORKSPACE_DIR/spike-rt"
BUILD_DIR="$SPIKE_RT_DIR/build"
OBJ_DIR="$BUILD_DIR/obj-primehub_$APP"
KERNEL_OBJ_DIR="$BUILD_DIR/obj-primehub_kernel"

echo "=== Building C '$APP' Application ==="

mkdir -p "$OBJ_DIR"
cd "$OBJ_DIR"

# Configure
ruby "$SPIKE_RT_DIR/asp3/configure.rb" \
    -T primehub_gcc \
    -L "$KERNEL_OBJ_DIR" \
    -a "$SPIKE_RT_DIR/sample/$APP" \
    -A "$APP" \
    -m "$SPIKE_RT_DIR/common/app.mk"

# Build
make clean
make -j$(nproc)

echo "=== Build Complete ==="
echo ""
echo "=== Flashing to Hub (Keep USB connected) ==="

BIN_PATH="asp.bin"

if [ ! -f "$BIN_PATH" ]; then
    echo "Error: Binary not found at $BIN_PATH"
    exit 1
fi

echo "Flashing $BIN_PATH..."
# Use alt interface 0 for internal flash
sudo dfu-util -d 0694:0011 -a 0 -s 0x08008000 -D "$BIN_PATH"

echo ""
echo "=== Done! ==="
echo "Press the center button on the Hub to start the program."
