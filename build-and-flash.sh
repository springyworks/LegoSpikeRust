#!/bin/bash
set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
CRATE_DIR="$SCRIPT_DIR/hub-motors"
BIN_NAME="hub-motors"

echo "=== Building bare-metal Rust motor control ==="
cd "$CRATE_DIR"
cargo build --release 2>&1

# Convert ELF to raw binary
ELF="$SCRIPT_DIR/target/thumbv7em-none-eabihf/release/$BIN_NAME"
BIN="$SCRIPT_DIR/target/thumbv7em-none-eabihf/release/$BIN_NAME.bin"

if [ ! -f "$ELF" ]; then
    echo "ERROR: ELF not found at $ELF"
    exit 1
fi

arm-none-eabi-objcopy -O binary "$ELF" "$BIN"

SIZE=$(stat -c%s "$BIN" 2>/dev/null || stat -f%z "$BIN")
echo "Binary size: $SIZE bytes"
echo ""

echo "=== Flashing to LEGO Hub via DFU ==="
echo "Make sure the Hub is in DFU mode:"
echo "  1. Unplug USB"
echo "  2. Hold Bluetooth button"
echo "  3. Plug USB while holding"
echo "  4. Wait for pink/purple pulse"
echo ""

# Try both known DFU device IDs
if lsusb 2>/dev/null | grep -q "0694:0011"; then
    DFU_ID="0694:0011"
elif lsusb 2>/dev/null | grep -q "0694:0008"; then
    DFU_ID="0694:0008"
else
    echo "No LEGO Hub in DFU mode detected."
    echo "Waiting for DFU device... (plug in hub in DFU mode)"
    for i in $(seq 1 30); do
        sleep 1
        if lsusb 2>/dev/null | grep -q "0694:0011"; then
            DFU_ID="0694:0011"; break
        elif lsusb 2>/dev/null | grep -q "0694:0008"; then
            DFU_ID="0694:0008"; break
        fi
        printf "."
    done
    echo ""
    if [ -z "$DFU_ID" ]; then
        echo "ERROR: Timed out waiting for DFU device."
        exit 1
    fi
fi

echo "Found DFU device: $DFU_ID"
echo "Flashing $BIN → 0x08008000..."
sudo dfu-util -d "$DFU_ID" -a 0 -s 0x08008000 -D "$BIN"

echo ""
echo "Resetting hub to run application..."
sudo dfu-util -d "$DFU_ID" -a 0 -s 0x08000000:leave -D /dev/null 2>/dev/null || true

echo ""
echo "=== Done! ==="
echo "Motors on Port A and Port B should start moving."
echo "To reflash: put hub in DFU mode and run this script again."
