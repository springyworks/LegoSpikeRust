#!/bin/bash
set -e

echo "=== USB-Connected Development Mode ==="
echo "Building Rust LED application..."

# Build the application
cd spike-rt && ./build_rust_led.sh && cd ..

echo ""
echo "=== Flashing to Hub (Keep USB connected) ==="

# Flash directly using raw binary (known working method)
BIN_PATH="spike-rt/build/obj-primehub_rust_led/asp.bin"

if [ ! -f "$BIN_PATH" ]; then
    echo "Error: Binary not found at $BIN_PATH"
    exit 1
fi

echo "Flashing $BIN_PATH..."
# Note: Must use alt interface 0 (internal flash), alt 1 doesn't have memory layout
sudo dfu-util -d 0694:0011 -a 0 -s 0x08008000 -D "$BIN_PATH"

echo ""
echo "=== Forcing Hub Reset ==="
echo "Sending reset command to exit DFU mode..."

# Send reset command to exit DFU mode and run application
sudo dfu-util -d 0694:0011 -a 0 -s 0x08000000:leave -D /dev/null 2>/dev/null || true

echo "Hub should now restart and run the Rust application!"

echo ""
echo "=== Development Instructions ==="
echo "✓ Flash complete - program will run automatically"
echo "✓ Keep USB connected for easy reflashing"  
echo "✓ Program runs for ~20 seconds then exits cleanly"
echo "✓ To reflash: just run ./deploy_rust_led.sh again"
echo "✓ No need to unplug USB or enter DFU mode manually"
