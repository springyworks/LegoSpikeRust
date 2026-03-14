#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "$0")" && pwd)"
PB_DIR="$ROOT_DIR/pybricks-micropython"
VENV_DIR="$PB_DIR/.venv-pybricks"
PROGRAM_FILE="${PROGRAM_FILE:-$ROOT_DIR/hub_smoke_test.py}"
POLL_SECONDS="${POLL_SECONDS:-2}"
RUN_ONCE="${RUN_ONCE:-0}"
AUTO_FLASH_ON_DFU="${AUTO_FLASH_ON_DFU:-0}"
INTERACTIVE="${INTERACTIVE:-1}"

# Known BLE hub name pattern (case-insensitive grep)
# Your hub advertises as "kosHUBpbricks" — we match "pybricks" or "pbricks" or "hub"
BLE_HUB_NAME="${BLE_HUB_NAME:-}"  # Set to exact name/address to skip scanning
BLE_NAME_PATTERNS="pbricks|pybricks"  # grep -iE pattern for BLE scan

# All known LEGO USB IDs (vendor 0694)
LEGO_VID="0694"
DFU_PIDS="0008 0011"
NORMAL_PIDS="0009 0010"

if [[ ! -d "$PB_DIR" ]]; then
  echo "ERROR: Missing directory: $PB_DIR"
  exit 1
fi

# --- State tracking (only log on changes) ---
PREV_STATE="unknown"
PREV_BLE_NAME=""
QUIET_TICKS=0

ts() { date "+%H:%M:%S"; }
log() { echo "[$(ts)] $*"; }

# ============================================================
# USB Detection: multi-method (sysfs + lsusb + ttyACM udevadm)
# ============================================================

scan_sysfs_lego() {
  for dev in /sys/bus/usb/devices/*/idVendor; do
    [[ -f "$dev" ]] || continue
    local vid
    vid="$(cat "$dev" 2>/dev/null)" || continue
    if [[ "$vid" == "$LEGO_VID" ]]; then
      cat "${dev%idVendor}idProduct" 2>/dev/null || true
    fi
  done
}

scan_lsusb_lego() {
  lsusb 2>/dev/null | grep -oP "0694:\K[0-9a-f]{4}" || true
}

scan_ttyacm_lego() {
  for tty in /dev/ttyACM*; do
    [[ -e "$tty" ]] || continue
    local info
    info="$(udevadm info "$tty" 2>/dev/null)" || continue
    if echo "$info" | grep -qi "ID_VENDOR_ID=$LEGO_VID"; then
      echo "$info" | grep -oP 'ID_MODEL_ID=\K.*' || true
    fi
  done
}

# ============================================================
# BLE Detection: bluetoothctl device list (no active scan needed
# if hub is already paired/discovered)
# ============================================================

# Returns "address name" of first matching BLE Pybricks hub, or empty
scan_ble_pybricks() {
  # If user set an explicit name/address, just echo it
  if [[ -n "$BLE_HUB_NAME" ]]; then
    echo "$BLE_HUB_NAME"
    return 0
  fi

  # Check already-known devices (no active scan — fast)
  local devices
  devices="$(bluetoothctl devices 2>/dev/null)" || return 0

  local match
  match="$(echo "$devices" | grep -iE "$BLE_NAME_PATTERNS" | head -1)" || true

  if [[ -n "$match" ]]; then
    # "Device AA:BB:CC:DD:EE:FF SomeName" → extract address and name
    local addr name
    addr="$(echo "$match" | awk '{print $2}')"
    name="$(echo "$match" | cut -d' ' -f3-)"
    echo "$addr $name"
  fi
}

# Quick background BLE scan refresh — runs for a few seconds to update device list
ble_refresh_scan() {
  # Start scan, wait briefly, stop — updates bluetoothctl's device cache
  ( bluetoothctl scan on 2>/dev/null & local pid=$!; sleep 3; kill $pid 2>/dev/null; bluetoothctl scan off 2>/dev/null ) &>/dev/null || true
}

# ============================================================
# Combined state detection
# Returns: "dfu:<pid>" | "usb:<pid>" | "ble:<addr>:<name>" | "lego_unknown:<pid>" | "none"
# ============================================================

detect_hub_state() {
  # --- USB detection ---
  local all_pids
  all_pids="$(scan_sysfs_lego) $(scan_lsusb_lego) $(scan_ttyacm_lego)"
  local unique_pids
  unique_pids="$(echo "$all_pids" | tr ' ' '\n' | grep -v '^$' | sort -u | tr '\n' ' ')"

  # DFU takes priority
  for pid in $DFU_PIDS; do
    for found in $unique_pids; do
      [[ "$found" == "$pid" ]] && { echo "dfu:$pid"; return 0; }
    done
  done

  # Normal USB
  for pid in $NORMAL_PIDS; do
    for found in $unique_pids; do
      [[ "$found" == "$pid" ]] && { echo "usb:$pid"; return 0; }
    done
  done

  # Unknown LEGO USB PID
  for found in $unique_pids; do
    [[ -n "$found" ]] && { echo "lego_unknown:$found"; return 0; }
  done

  # --- BLE detection ---
  local ble_result
  ble_result="$(scan_ble_pybricks)"
  if [[ -n "$ble_result" ]]; then
    local ble_addr ble_name
    ble_addr="$(echo "$ble_result" | awk '{print $1}')"
    ble_name="$(echo "$ble_result" | cut -d' ' -f2-)"
    echo "ble:$ble_addr:$ble_name"
    return 0
  fi

  echo "none"
}

# ============================================================
# Actions
# ============================================================

ensure_pybricksdev() {
  if [[ -x "$VENV_DIR/bin/pybricksdev" ]]; then return 0; fi
  log "Creating local Python venv for pybricksdev..."
  python3 -m venv "$VENV_DIR"
  log "Installing pybricksdev..."
  "$VENV_DIR/bin/pip" install pybricksdev
}

ensure_program_file() {
  if [[ -f "$PROGRAM_FILE" ]]; then return 0; fi
  log "Creating default smoke-test program: $PROGRAM_FILE"
  cat >"$PROGRAM_FILE" <<'PY'
from pybricks.hubs import PrimeHub
from pybricks.parameters import Color
from pybricks.tools import wait

hub = PrimeHub()
hub.display.char("H")
hub.light.on(Color.GREEN)
wait(1200)
hub.light.off()
print("Smoke test OK")
PY
}

flash_firmware_interactive() {
  local do_flash="n"
  if [[ "$AUTO_FLASH_ON_DFU" == "1" ]]; then
    do_flash="y"
  elif [[ "$INTERACTIVE" == "1" ]]; then
    read -r -t 30 -p "[$(ts)] DFU detected. Flash firmware now? [y/N]: " do_flash || do_flash="n"
  fi
  if [[ "$do_flash" =~ ^[Yy]$ ]]; then
    ( cd "$PB_DIR"; PATH="$VENV_DIR/bin:$PATH" make -C bricks/primehub -j"$(nproc)" deploy )
    log "Firmware flash finished."
  else
    log "Skipping flash."
  fi
}

run_program_usb() {
  log "Running via USB: $PROGRAM_FILE"
  ( cd "$PB_DIR"; PATH="$VENV_DIR/bin:$PATH" pybricksdev run usb "$PROGRAM_FILE" --start --wait )
}

run_program_ble() {
  local addr="$1"
  local name="$2"
  log "Running via BLE: $PROGRAM_FILE → $name ($addr)"
  # Use auto-discover (no -n) — more reliable than specifying random BLE addresses.
  # pybricksdev scans for Pybricks service UUID, which is the most reliable method.
  ( cd "$PB_DIR"; PATH="$VENV_DIR/bin:$PATH" pybricksdev run ble "$PROGRAM_FILE" --start --wait )
}

# ============================================================
# Main loop
# ============================================================

show_banner() {
  echo
  log "=== Hub Watcher Started ==="
  log "  Monitoring: USB (lsusb + sysfs + ttyACM) + Bluetooth LE"
  log "  BLE patterns: $BLE_NAME_PATTERNS"
  log "  USB VID: $LEGO_VID | DFU PIDs: $DFU_PIDS | Normal PIDs: $NORMAL_PIDS"
  log "  Poll: every ${POLL_SECONDS}s | Program: $(basename "$PROGRAM_FILE")"
  [[ -n "$BLE_HUB_NAME" ]] && log "  BLE hub override: $BLE_HUB_NAME"
  log "  Ctrl+C to stop"
  echo
}

BLE_SCAN_COUNTER=0

main_loop() {
  show_banner

  while true; do
    local state
    state="$(detect_hub_state)"

    # Every 5 iterations when no hub found, trigger a background BLE refresh
    if [[ "$state" == "none" ]]; then
      BLE_SCAN_COUNTER=$((BLE_SCAN_COUNTER + 1))
      if (( BLE_SCAN_COUNTER % 5 == 0 )); then
        ble_refresh_scan &
      fi
    else
      BLE_SCAN_COUNTER=0
    fi

    # --- State changed? ---
    if [[ "$state" != "$PREV_STATE" ]]; then
      QUIET_TICKS=0

      case "$state" in
        dfu:*)
          local pid="${state#dfu:}"
          log ">>> HUB — DFU mode (USB PID=$pid)"
          flash_firmware_interactive
          ;;
        usb:*)
          local pid="${state#usb:}"
          log ">>> HUB — Normal USB mode (PID=$pid)"
          if run_program_usb; then
            log "Program completed OK."
          else
            log "Program run failed (hub may have disconnected)."
          fi
          ;;
        ble:*)
          # Parse "ble:AA:BB:CC:DD:EE:FF:SomeName"
          local rest="${state#ble:}"
          # Address is first 17 chars (AA:BB:CC:DD:EE:FF), name is after the next colon
          local addr="${rest:0:17}"
          local name="${rest:18}"
          log ">>> HUB — Bluetooth LE: $name ($addr)"
          PREV_BLE_NAME="$name"
          if run_program_ble "$addr" "$name"; then
            log "Program completed OK via BLE."
          else
            log "BLE program run failed. Hub might have disconnected."
          fi
          ;;
        lego_unknown:*)
          local pid="${state#lego_unknown:}"
          log ">>> LEGO USB device (unknown PID=$pid) — try pressing center button."
          ;;
        none)
          if [[ "$PREV_STATE" != "unknown" && "$PREV_STATE" != "none" ]]; then
            log "<<< Hub disconnected."
          fi
          log "Waiting... (power on hub OR plug USB)"
          ;;
      esac

      PREV_STATE="$state"
    else
      # Same state — quiet mode, print status every 30s
      QUIET_TICKS=$((QUIET_TICKS + 1))
      local dot_interval=$(( 30 / POLL_SECONDS ))
      (( dot_interval < 1 )) && dot_interval=1
      if (( QUIET_TICKS % dot_interval == 0 )); then
        local label="$state"
        [[ "$state" == "none" ]] && label="waiting"
        printf "[%s] . (%s)\n" "$(ts)" "$label"
      fi
    fi

    [[ "$RUN_ONCE" == "1" && "$state" != "none" ]] && break

    sleep "$POLL_SECONDS"
  done
}

ensure_pybricksdev
ensure_program_file
main_loop
