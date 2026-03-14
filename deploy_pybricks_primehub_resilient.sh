#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "$0")" && pwd)"
PB_DIR="$ROOT_DIR/pybricks-micropython"
VENV_DIR="$PB_DIR/.venv-pybricks"
DFU_TIMEOUT_SECONDS="${DFU_TIMEOUT_SECONDS:-180}"
RETRY_COUNT="${RETRY_COUNT:-3}"

if [[ ! -d "$PB_DIR" ]]; then
    echo "ERROR: Missing directory: $PB_DIR"
    exit 1
fi

log() {
    echo "[pybricks-deploy] $*"
}

detect_dfu_id() {
    if lsusb 2>/dev/null | grep -q "0694:0011"; then
        echo "0694:0011"
        return 0
    fi

    if lsusb 2>/dev/null | grep -q "0694:0008"; then
        echo "0694:0008"
        return 0
    fi

    return 1
}

detect_any_lego_usb() {
    lsusb 2>/dev/null | grep -E "0694:" >/dev/null 2>&1
}

ensure_pybricksdev() {
    if [[ -x "$VENV_DIR/bin/pybricksdev" ]]; then
        return 0
    fi

    log "Creating local Python venv for pybricksdev..."
    python3 -m venv "$VENV_DIR"

    log "Installing pybricksdev in local venv..."
    "$VENV_DIR/bin/pip" install pybricksdev
}

wait_for_dfu() {
    local timeout="$1"
    local elapsed=0

    log "Waiting for LEGO hub in DFU mode (timeout: ${timeout}s)..."

    while (( elapsed < timeout )); do
        local dfu_id
        if dfu_id="$(detect_dfu_id)"; then
            log "Detected DFU device: $dfu_id"
            return 0
        fi

        if (( elapsed == 0 || elapsed % 15 == 0 )); then
            echo
            log "No DFU device yet. If hub is only charging, it may still be powered off."
            if detect_any_lego_usb; then
                log "LEGO USB device detected, but not in DFU mode."
                log "If the hub is off (charging icon/light only), press the center button to power it on."
            fi
            log "Recovery sequence:"
            log "  1) Unplug USB"
            log "  2) Hold center button 10-15s to fully power off"
            log "  3) Hold Bluetooth button (top-right)"
            log "  4) Plug USB while still holding Bluetooth"
            log "  5) Release when LED pulses purple/pink"
            log "Tip: You only need DFU for firmware flashing."
            log "For normal development, keep firmware installed and use pybricksdev run (usb/ble)."
            echo
        fi

        sleep 1
        ((elapsed++))

        if (( elapsed % 5 == 0 )); then
            printf "."
        fi
    done

    echo
    log "Timed out waiting for DFU device."
    return 1
}

deploy_once() {
    (
        cd "$PB_DIR"
        PATH="$VENV_DIR/bin:$PATH" make -C bricks/primehub -j"$(nproc)" deploy
    )
}

main() {
    ensure_pybricksdev

    local attempt=1
    while (( attempt <= RETRY_COUNT )); do
        log "Deploy attempt $attempt/$RETRY_COUNT"

        if ! wait_for_dfu "$DFU_TIMEOUT_SECONDS"; then
            if (( attempt == RETRY_COUNT )); then
                log "Giving up after $RETRY_COUNT attempts."
                exit 1
            fi
            ((attempt++))
            continue
        fi

        if deploy_once; then
            log "Firmware deployment succeeded."
            log "If the hub does not auto-reboot, power-cycle once."
            exit 0
        fi

        log "Deploy command failed. You can re-enter DFU and retry automatically."
        ((attempt++))
    done

    exit 1
}

main "$@"
