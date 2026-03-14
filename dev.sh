#!/usr/bin/env bash
# ═══════════════════════════════════════════════════════════════
# dev.sh — One-command Rust dev cycle for LEGO SPIKE Prime Hub
#
# Usage:
#   ./dev.sh                   # build hub-motors + flash + run
#   ./dev.sh hub-motors        # specify crate directory
#   ./dev.sh --watch            # watch mode: auto-rebuild+flash on code changes
#   SKIP_BUILD=1 ./dev.sh      # re-flash last binary (no rebuild)
#   CHECK_ONLY=1 ./dev.sh      # cargo check only (fast, no flash)
#
# The script handles EVERYTHING:
#   1. Compiles your Rust code (cross-compile for Cortex-M4F)
#   2. Converts ELF → raw binary
#   3. Detects hub state (USB DFU / USB normal / BLE / off / gone)
#   4. Guides you to DFU mode with minimal instructions
#   5. Flashes via dfu-util
#   6. Resets hub to run your code
#
# Developer just: edit code → run ./dev.sh → watch hub LEDs
# ═══════════════════════════════════════════════════════════════
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "$0")" && pwd)"

# Parse arguments
WATCH_MODE=0
BOOTLOADER_MODE=0
MONITOR_MODE=0
CRATE_DIR="hub-motors"
for arg in "$@"; do
  case "$arg" in
    --watch|-w) WATCH_MODE=1 ;;
    --bootloader|--bl) BOOTLOADER_MODE=1 ;;
    --monitor|--mon) MONITOR_MODE=1 ;;
    *) CRATE_DIR="$arg" ;;
  esac
done

CRATE_PATH="$ROOT_DIR/$CRATE_DIR"
SKIP_BUILD="${SKIP_BUILD:-0}"
CHECK_ONLY="${CHECK_ONLY:-0}"  # CHECK_ONLY=1 → cargo check only, no flash

# Timing
DFU_WAIT_TIMEOUT="${DFU_WAIT_TIMEOUT:-120}"  # seconds to wait for DFU

# LEGO USB identifiers
LEGO_VID="0694"
DFU_PIDS="0008 0011"
NORMAL_PIDS="0009 0010"
# STM32 system bootloader DFU (entered via magic RAM jump)
ST_VID="0483"
ST_DFU_PID="df11"
BLE_NAME_PATTERNS="pbricks|pybricks|prime.hub|spike|lego|mindstorms"

# Flash addresses
BOOTLOADER_FLASH_ADDR="0x08008000"  # Bootloader (flashed via LEGO DFU)
APP_FLASH_ADDR="0x08010000"         # Application (flashed via STM32 system DFU)
FLASH_ADDR="$APP_FLASH_ADDR"       # Default: flash app

# ─── Helpers ─────────────────────────────────────────────────

RED='\033[0;31m'; GRN='\033[0;32m'; YEL='\033[1;33m'; BLU='\033[0;34m'
CYN='\033[0;36m'; BLD='\033[1m'; DIM='\033[2m'; RST='\033[0m'

ts() { date "+%H:%M:%S"; }
info()  { printf "${BLU}[%s]${RST} %s\n" "$(ts)" "$*"; }
ok()    { printf "${GRN}[%s] ✓${RST} %s\n" "$(ts)" "$*"; }
warn()  { printf "${YEL}[%s] !${RST} %s\n" "$(ts)" "$*"; }
err()   { printf "${RED}[%s] ✗${RST} %s\n" "$(ts)" "$*"; }
bold()  { printf "${BLD}%s${RST}" "$*"; }
dim()   { printf "${DIM}%s${RST}" "$*"; }
action(){ printf "  ${CYN}→ ${RST}%b\n" "$*"; }

# ─── User Attention System ───────────────────────────────────
#
# Escalation tiers:
#   1. Terminal text (always)
#   2. Desktop toast notification (5s disappears)
#   3. Persistent popup dialog + alert sound
#   4. Repeated sound every 10s until user responds
#
# Works on: X11, Wayland, GNOME, KDE, headless (degrades gracefully)

ALERT_SOUND="/usr/share/sounds/gnome/default/alerts/swing.ogg"
ALERT_SOUND_URGENT="/usr/share/sounds/gnome/default/alerts/hum.ogg"

# Tier 1+2: non-blocking desktop toast notification
notify_user() {
  local urgency="${1:-normal}"  # low|normal|critical
  local title="$2"
  local body="$3"
  # Desktop notification (non-blocking)
  if command -v notify-send &>/dev/null; then
    notify-send --urgency="$urgency" \
      --icon=dialog-information \
      --app-name="LEGO Dev" \
      "$title" "$body" 2>/dev/null &
  fi
}

# Tier 3: blocking popup dialog — steals focus, user MUST respond
#   Returns 0 if user clicked OK, 1 if they closed/cancelled
popup_attention() {
  local title="$1"
  local body="$2"
  # Also send a desktop notification in case zenity doesn't get focus
  notify_user "critical" "$title" "$body"
  # Play alert sound
  play_sound "$ALERT_SOUND" &
  # Modal dialog — blocks until user responds
  if command -v zenity &>/dev/null; then
    zenity --info --title="$title" --text="$body" \
      --width=400 --ok-label="Got it" 2>/dev/null
    return $?
  elif command -v xmessage &>/dev/null; then
    xmessage -center -buttons "Got it:0" "$title\n\n$body" 2>/dev/null
    return $?
  else
    # Fallback: just beep
    printf '\a'
    return 0
  fi
}

# Tier 4: escalating sound alert — repeat until stopped
ESCALATION_PID=""

start_escalation() {
  local msg="$1"
  (
    local count=0
    while true; do
      play_sound "$ALERT_SOUND_URGENT"
      notify_user "critical" "🔴 LEGO Hub — Action Required" "$msg"
      sleep 10
      count=$((count + 1))
      # After 3 rounds, also try system bell
      if (( count >= 3 )); then printf '\a'; fi
    done
  ) &
  ESCALATION_PID=$!
}

stop_escalation() {
  if [[ -n "$ESCALATION_PID" ]]; then
    kill "$ESCALATION_PID" 2>/dev/null || true
    wait "$ESCALATION_PID" 2>/dev/null || true
    ESCALATION_PID=""
  fi
}

play_sound() {
  local snd="${1:-$ALERT_SOUND}"
  if [[ -f "$snd" ]] && command -v paplay &>/dev/null; then
    paplay "$snd" 2>/dev/null &
  elif command -v aplay &>/dev/null; then
    # Generate a short beep via ALSA
    aplay -q /usr/share/sounds/alsa/Front_Center.wav 2>/dev/null &
  else
    printf '\a'  # terminal bell fallback
  fi
}

# ─── Validation ──────────────────────────────────────────────

validate_setup() {
  local missing=0
  for cmd in cargo arm-none-eabi-objcopy dfu-util; do
    if ! command -v "$cmd" &>/dev/null; then
      err "Missing: $cmd"
      missing=1
    fi
  done
  if [[ ! -d "$CRATE_PATH" ]]; then
    err "Crate directory not found: $CRATE_PATH"
    err "Usage: $0 <crate-dir>  (e.g. hub-motors)"
    exit 1
  fi
  if [[ ! -f "$CRATE_PATH/Cargo.toml" ]]; then
    err "No Cargo.toml in $CRATE_PATH"
    exit 1
  fi
  if [[ $missing -eq 1 ]]; then exit 1; fi
}

# ─── Step 1: BUILD ───────────────────────────────────────────

build_rust() {
  if [[ "$SKIP_BUILD" == "1" ]]; then
    info "Skipping build (SKIP_BUILD=1)"
    return 0
  fi

  local crate_name
  crate_name="$(basename "$CRATE_DIR")"

  echo
  if [[ "$CHECK_ONLY" == "1" ]]; then
    info "$(bold 'CHECK') — $crate_name"
    if ! ( cd "$CRATE_PATH" && cargo check 2>&1 ); then
      err "Check failed."
      exit 1
    fi
    ok "No errors"
    exit 0
  fi

  info "$(bold 'COMPILE') — $crate_name"

  # Build from crate dir so .cargo/config.toml picks up the right target
  if ! ( cd "$CRATE_PATH" && cargo build --release 2>&1 ); then
    err "Compilation failed. Fix errors above and re-run."
    exit 1
  fi
  ok "Compiled"
}

# ─── Step 2: ELF → BIN ──────────────────────────────────────

make_binary() {
  local crate_name
  crate_name="$(basename "$CRATE_DIR")"
  ELF_PATH="$ROOT_DIR/target/thumbv7em-none-eabihf/release/$crate_name"
  BIN_PATH="${ELF_PATH}.bin"

  if [[ ! -f "$ELF_PATH" ]]; then
    err "ELF not found: $ELF_PATH"
    exit 1
  fi

  arm-none-eabi-objcopy -O binary "$ELF_PATH" "$BIN_PATH"
  local size
  size=$(stat -c%s "$BIN_PATH" 2>/dev/null || stat -f%z "$BIN_PATH")
  ok "Binary: ${size} bytes → $(dim "$BIN_PATH")"
}

# ─── Hub detection ───────────────────────────────────────────

# Scan USB for LEGO vendor, return PIDs found (fast — sysfs only)
scan_usb_lego_pids() {
  local pids=""
  for dev in /sys/bus/usb/devices/*/idVendor; do
    [[ -f "$dev" ]] || continue
    local vid
    vid="$(cat "$dev" 2>/dev/null)" || continue
    if [[ "$vid" == "$LEGO_VID" ]]; then
      pids="$pids $(cat "${dev%idVendor}idProduct" 2>/dev/null || true)"
    fi
  done
  echo "$pids" | tr ' ' '\n' | grep -v '^$' | sort -u | tr '\n' ' '
}

# Check if hub is visible on BLE
scan_ble_hub() {
  # Check already-known devices first (fast)
  local found
  found="$(bluetoothctl devices 2>/dev/null | grep -iE "$BLE_NAME_PATTERNS" | head -1 | awk '{print $2, $3}')" || true
  if [[ -n "$found" ]]; then
    echo "$found"
    return
  fi
  # Quick live scan (2s) to discover new advertisements
  timeout 3 bluetoothctl --timeout 2 scan on &>/dev/null || true
  bluetoothctl devices 2>/dev/null | grep -iE "$BLE_NAME_PATTERNS" | head -1 | awk '{print $2, $3}' || true
}

# ── USB port baseline: snapshot which ports have devices ─────
#
# When the hub is off, its USB PHY is unpowered → D+/D- lines
# are not pulled up → the host controller sees "not attached",
# indistinguishable from "no cable".  Current draw on VBUS is
# not exposed via sysfs on standard USB host controllers.
#
# What we CAN do: take a baseline of "configured" ports at
# startup.  If a NEW port transitions to "configured", we know
# something was just plugged in (or powered on) even before
# full enumeration completes.  This is faster than lsusb polling.

USB_BASELINE_PORTS=""

# Snapshot configured USB ports → space-separated port paths
snapshot_usb_ports() {
  local ports=""
  for state_file in /sys/bus/usb/devices/usb*/*/usb*-port*/state; do
    [[ -f "$state_file" ]] || continue
    if [[ "$(cat "$state_file" 2>/dev/null)" == "configured" ]]; then
      ports="$ports ${state_file%/state}"
    fi
  done
  echo "$ports"
}

# Check if any NEW USB port appeared since baseline
new_usb_port_appeared() {
  local current
  current="$(snapshot_usb_ports)"
  for port in $current; do
    echo "$USB_BASELINE_PORTS" | grep -qF "$port" || { echo "$port"; return 0; }
  done
  return 1
}

# ── udevadm monitor helper ──────────────────────────────────
# Run in background during wait loop for instant USB detection.
UDEV_MON_PID=""
UDEV_MON_FIFO=""

start_udev_monitor() {
  UDEV_MON_FIFO=$(mktemp -u /tmp/dev-sh-udev.XXXXXX)
  mkfifo "$UDEV_MON_FIFO" 2>/dev/null || return 1
  # Monitor only USB subsystem, write to FIFO
  udevadm monitor --subsystem-match=usb --property \
    2>/dev/null > "$UDEV_MON_FIFO" &
  UDEV_MON_PID=$!
}

stop_udev_monitor() {
  [[ -n "$UDEV_MON_PID" ]] && kill "$UDEV_MON_PID" 2>/dev/null || true
  UDEV_MON_PID=""
  [[ -n "$UDEV_MON_FIFO" ]] && rm -f "$UDEV_MON_FIFO" 2>/dev/null || true
  UDEV_MON_FIFO=""
}

# Check if udevadm caught a LEGO USB event (non-blocking)
check_udev_lego_event() {
  [[ -n "$UDEV_MON_FIFO" ]] || return 1
  # Non-blocking read — check if FIFO has data with LEGO VID
  timeout 0.1 cat "$UDEV_MON_FIFO" 2>/dev/null | grep -qi "$LEGO_VID" && return 0
  return 1
}

# ── Deep hardware awareness ─────────────────────────────────
# Squeeze every bit of info from the OS to understand hub state.

# Kernel USB event log — watch journalctl for USB plug/unplug
JOURNAL_MON_PID=""
JOURNAL_MON_FILE=""

start_journal_monitor() {
  JOURNAL_MON_FILE=$(mktemp /tmp/dev-sh-journal.XXXXXX)
  journalctl -k --since="now" -f --no-pager -g "usb|USB" \
    2>/dev/null > "$JOURNAL_MON_FILE" &
  JOURNAL_MON_PID=$!
}

stop_journal_monitor() {
  [[ -n "$JOURNAL_MON_PID" ]] && kill "$JOURNAL_MON_PID" 2>/dev/null || true
  JOURNAL_MON_PID=""
  [[ -n "$JOURNAL_MON_FILE" ]] && rm -f "$JOURNAL_MON_FILE" 2>/dev/null || true
  JOURNAL_MON_FILE=""
}

# Check if kernel logged any new USB activity (plug, disconnect, overcurrent)
check_kernel_usb_events() {
  [[ -n "$JOURNAL_MON_FILE" && -f "$JOURNAL_MON_FILE" ]] || return 1
  local events
  events="$(cat "$JOURNAL_MON_FILE" 2>/dev/null)"
  [[ -n "$events" ]] || return 1
  # Return relevant line
  echo "$events" | tail -1
  # Clear for next check
  : > "$JOURNAL_MON_FILE" 2>/dev/null
  return 0
}

# Get BLE RSSI for hub proximity estimate
get_ble_rssi() {
  local addr="$1"
  [[ -n "$addr" ]] || return 1
  # hcitool for RSSI (if connected)
  if command -v hcitool &>/dev/null; then
    hcitool rssi "$addr" 2>/dev/null | grep -oP '[-0-9]+' || true
  fi
}

# Summary of all USB hardware state — for diagnostics
hw_diag() {
  local n_ports=0 n_configured=0
  for state_file in /sys/bus/usb/devices/usb*/*/usb*-port*/state; do\
    [[ -f "$state_file" ]] || continue
    n_ports=$((n_ports + 1))
    [[ "$(cat "$state_file" 2>/dev/null)" == "configured" ]] && n_configured=$((n_configured + 1))
  done
  printf "${DIM}  hw: %d/%d USB ports active" "$n_configured" "$n_ports"
  # BLE adapter state
  if command -v bluetoothctl &>/dev/null; then
    local bt_power
    bt_power="$(bluetoothctl show 2>/dev/null | grep "Powered:" | awk '{print $2}')"
    printf ", BT %s" "${bt_power:-?}"
  fi
  printf "${RST}\n"
}

# Fast USB-only detection (sub-millisecond, no BLE scan)
# Returns: dfu | usb_normal | charging | none
detect_hub_usb() {
  local usb_pids
  usb_pids="$(scan_usb_lego_pids)"

  # DFU mode? (highest priority — we can flash)
  for pid in $DFU_PIDS; do
    echo "$usb_pids" | grep -qw "$pid" && { echo "dfu"; return; }
  done

  # STM32 system bootloader DFU?
  for dev in /sys/bus/usb/devices/*/idVendor; do
    [[ -f "$dev" ]] || continue
    [[ "$(cat "$dev" 2>/dev/null)" == "$ST_VID" ]] || continue
    [[ "$(cat "${dev%idVendor}idProduct" 2>/dev/null)" == "$ST_DFU_PID" ]] && {
      echo "dfu"
      return
    }
  done

  # Normal USB?
  for pid in $NORMAL_PIDS; do
    echo "$usb_pids" | grep -qw "$pid" && { echo "usb_normal"; return; }
  done

  # Any LEGO VID on USB?
  if [[ -n "$(echo "$usb_pids" | tr -d ' ')" ]]; then
    echo "charging"
    return
  fi

  echo "none"
}

# Full detection including BLE (slow — 2-3 seconds for BLE scan)
# Returns: dfu | usb_normal | ble | charging | none
detect_hub() {
  local usb_result
  usb_result="$(detect_hub_usb)"
  [[ "$usb_result" != "none" ]] && { echo "$usb_result"; return; }

  # BLE? (only if USB found nothing)
  local ble
  ble="$(scan_ble_hub)"
  if [[ -n "$ble" ]]; then
    echo "ble"
    return
  fi

  echo "none"
}

# Get the right DFU device ID for dfu-util
get_dfu_device_id() {
  # Check LEGO DFU first
  for pid in $DFU_PIDS; do
    for dev in /sys/bus/usb/devices/*/idVendor; do
      [[ -f "$dev" ]] || continue
      [[ "$(cat "$dev" 2>/dev/null)" == "$LEGO_VID" ]] || continue
      [[ "$(cat "${dev%idVendor}idProduct" 2>/dev/null)" == "$pid" ]] && {
        echo "${LEGO_VID}:${pid}"
        return
      }
    done
  done
  # Check STM32 system bootloader DFU
  for dev in /sys/bus/usb/devices/*/idVendor; do
    [[ -f "$dev" ]] || continue
    [[ "$(cat "$dev" 2>/dev/null)" == "$ST_VID" ]] || continue
    [[ "$(cat "${dev%idVendor}idProduct" 2>/dev/null)" == "$ST_DFU_PID" ]] && {
      echo "${ST_VID}:${ST_DFU_PID}"
      return
    }
  done
}

# ─── Step 3: GET HUB INTO DFU ───────────────────────────────

#  Elaborate decision tree — detects the hub's current state and
#  walks the user through ONE step at a time, reacting to every
#  state transition along the way.
#
#  Detectable states (from detect_hub):
#    dfu          → ready to flash
#    usb_normal   → hub powered on, running firmware over USB
#    ble          → hub powered on, visible on Bluetooth only
#    charging     → LEGO VID on USB but unknown PID
#    none         → nothing visible (cable missing OR hub off)
#
#  Phases (internal — what instruction we last gave):
#    start              → initial assessment
#    need_cable         → told user to plug USB
#    need_poweron       → told user to press center button
#    need_shutdown      → told user to hold center 3s to power off
#    need_dfu_entry     → told user to hold BT + replug USB
#    need_usb_from_ble  → BLE-visible hub, told user to plug USB

wait_for_dfu() {
  local state
  state="$(detect_hub_usb)"

  if [[ "$state" == "dfu" ]]; then
    ok "Hub in DFU mode"
    play_sound "$ALERT_SOUND" &
    notify_user "normal" "LEGO Hub Ready" "Hub is in DFU mode — flashing now"
    return 0
  fi

  # Full detect (including BLE) for initial state
  state="$(detect_hub)"

  if [[ "$state" == "dfu" ]]; then
    ok "Hub in DFU mode"
    play_sound "$ALERT_SOUND" &
    notify_user "normal" "LEGO Hub Ready" "Hub is in DFU mode — flashing now"
    return 0
  fi

  echo
  info "$(bold 'HUB SETUP') — getting hub into DFU mode"

  # Take USB port baseline — we'll detect NEW connections by diffing
  USB_BASELINE_PORTS="$(snapshot_usb_ports)"
  local n_baseline
  n_baseline=$(echo "$USB_BASELINE_PORTS" | wc -w)
  dim "  (baseline: ${n_baseline} USB devices already connected)"
  echo
  hw_diag

  # Start background monitors for instant detection
  start_udev_monitor
  start_journal_monitor
  trap 'stop_udev_monitor; stop_journal_monitor; stop_escalation' EXIT

  local prev_state=""
  local phase="start"
  local start_time
  start_time=$(date +%s)
  local elapsed=0
  local phase_start=0
  local phase_time=0
  local notified_tier=0  # 0=text only, 1=toast sent, 2=popup sent, 3=escalating
  local state_changes=0
  local last_state_change_time=0
  local last_scan_msg=0
  local last_ble_check=0
  local ble_check_interval=10  # seconds between BLE scans
  local instructions_given=false  # track if we showed ANY instruction box

  while [[ "$state" != "dfu" ]]; do
    elapsed=$(( $(date +%s) - start_time ))
    phase_time=$(( $(date +%s) - phase_start ))

    # ── React to state changes ───────────────────────────────
    if [[ "$state" != "$prev_state" ]]; then
      phase_start=$(date +%s)
      phase_time=0

      # Flap detection: too many state changes in a short time?
      state_changes=$((state_changes + 1))
      local now_ts
      now_ts=$(date +%s)
      if (( state_changes > 3 && (now_ts - last_state_change_time) < 10 )); then
        echo
        warn "Hub state is bouncing around — stop pressing buttons for a moment"
        dim "  (waiting 5s for things to settle...)"
        echo
        sleep 5
        state="$(detect_hub_usb)"
        if [[ "$state" == "dfu" ]]; then break; fi
        state_changes=0
      fi
      last_state_change_time=$now_ts

      case "$state" in

        # ── Hub running on USB ─────────────────────────────
        usb_normal)
          echo
          ok "Hub detected on USB — powered on, running firmware"
          play_sound "$ALERT_SOUND" &
          notify_user "normal" "LEGO Hub Found" "Hub on USB — trying to switch to firmware-update mode..."
          phase="need_auto_dfu"
          echo
          info "Attempting automatic switch to firmware-update mode..."
          instructions_given=true
          # Try programmatic DFU entry first — no manual work needed
          local auto_dfu_ok=false
          # Attempt 1: dfu-util --detach (works if firmware exposes DFU runtime)
          if command -v dfu-util >/dev/null 2>&1; then
            dfu-util -d 0694:0009,0694:0010 --detach 2>/dev/null && auto_dfu_ok=true || true
          fi
          if [[ "$auto_dfu_ok" == true ]]; then
            ok "Sent firmware-update request — waiting for hub to reappear..."
            sleep 3
            state="$(detect_hub_usb)"
            if [[ "$state" == "dfu" ]]; then
              break  # Success — no manual steps needed!
            fi
          fi
          # Auto-DFU didn't work — fall back to simple instructions
          phase="need_shutdown"
          echo
          printf "  ${BLD}Almost there — one quick step:${RST}\n"
          action "Hold the ${BLD}center button${RST}${CYN} for 3 seconds to turn the hub off"
          printf "  ${DIM}(all LEDs will go dark when it's off)${RST}\n"
          echo
          info "Waiting for hub to power off..."
          ;;

        # ── Hub visible on BLE (no USB) ────────────────────
        ble)
          echo
          ok "Hub detected on Bluetooth — powered on wirelessly"
          play_sound "$ALERT_SOUND" &
          notify_user "normal" "LEGO Hub Found (BLE)" "Hub on Bluetooth — plug USB cable to continue"
          phase="need_usb_from_ble"
          instructions_given=true
          echo
          printf "  ${BLD}Next step:${RST}\n"
          action "Plug the USB cable into the hub (port at the top)"
          echo
          info "Waiting for USB connection..."
          ;;

        # ── LEGO VID present but unknown PID ───────────────
        charging)
          echo
          ok "Hub detected on USB (charging or unknown state)"
          phase="need_shutdown"
          instructions_given=true
          echo
          printf "  ${BLD}Next step:${RST}\n"
          action "Hold the ${BLD}center button${RST}${CYN} for 3 seconds to turn it off"
          action "Then we'll guide you through the next step"
          echo
          info "Waiting for hub to power off..."
          ;;

        # ── Nothing visible ────────────────────────────────
        none)
          if [[ "$prev_state" == "usb_normal" || "$prev_state" == "charging" ]]; then
            # User just shut down the hub (or unplugged) — proceed to DFU entry
            echo
            ok "Hub powered off — good"
            play_sound "$ALERT_SOUND" &
            notify_user "normal" "LEGO Hub" "Hub off — one more step"
            phase="need_dfu_entry"
            instructions_given=true
            echo
            printf "  ${BLD}Last step — firmware-update mode:${RST}\n"
            action "1. Unplug the USB cable"
            action "2. Hold the small ${BLD}Bluetooth button${RST}${CYN} (near USB port)"
            action "3. While holding it, plug USB back in"
            action "4. Wait for pink/purple LED, then let go"
            echo
            info "Waiting for firmware-update mode..."

          elif [[ "$prev_state" == "ble" ]]; then
            # Hub went off BLE — probably shut down
            echo
            warn "Hub went offline from Bluetooth"
            phase="need_cable"
            instructions_given=true
            echo
            action "Plug USB cable in and press the center button to power on"
            echo
            info "Scanning..."

          elif [[ -z "$prev_state" || "$prev_state" == "none" ]]; then
            # ── Cold start: nothing detected at all ──
            phase="need_cable"
            echo
            warn "No hub detected on USB or Bluetooth"
            echo
            # Comprehensive initial instructions covering ALL possibilities
            printf "  ${YEL}┌──────────────────────────────────────────────────────────────┐${RST}\n"
            printf "  ${YEL}│${RST}                                                              ${YEL}│${RST}\n"
            printf "  ${YEL}│${RST}  ${BLD}If hub is OFF or on battery:${RST}                              ${YEL}│${RST}\n"
            printf "  ${YEL}│${RST}    1. Plug USB cable into the hub (top port)                  ${YEL}│${RST}\n"
            printf "  ${YEL}│${RST}    2. Press the center button once to power on                ${YEL}│${RST}\n"
            printf "  ${YEL}│${RST}                                                              ${YEL}│${RST}\n"
            printf "  ${YEL}│${RST}  ${BLD}If hub has blinking BT LED (already in DFU):${RST}              ${YEL}│${RST}\n"
            printf "  ${YEL}│${RST}    Just plug in the USB cable — DFU needs USB to work         ${YEL}│${RST}\n"
            printf "  ${YEL}│${RST}                                                              ${YEL}│${RST}\n"
            printf "  ${YEL}│${RST}    ${DIM}(the script will auto-detect it the moment USB connects)${RST} ${YEL}│${RST}\n"
            printf "  ${YEL}│${RST}                                                              ${YEL}│${RST}\n"
            printf "  ${YEL}└──────────────────────────────────────────────────────────────┘${RST}\n"
            echo
            instructions_given=true
            info "Scanning USB + Bluetooth..."

          else
            # Came from some other state (unexpected transition)
            echo
            warn "Hub disappeared (was: ${prev_state})"
            phase="need_cable"
            instructions_given=true
            echo
            action "Plug USB cable into the hub and ensure it's powered on"
            printf "  ${DIM}If the hub has a blinking colored LED (DFU mode), just plug USB in${RST}\n"
            echo
            info "Scanning..."
          fi
          ;;
      esac

      prev_state="$state"

      # ── Stop any escalation when state changes ────────────
      stop_escalation
      notified_tier=0
    fi

    # ── Phase-specific progressive hints + notification escalation ──

    # Check for new USB ports (fast sysfs diff)
    local new_port=""
    new_port="$(new_usb_port_appeared 2>/dev/null || true)"

    # Check kernel log for USB events (non-blocking)
    local kern_event=""
    kern_event="$(check_kernel_usb_events 2>/dev/null || true)"
    if [[ -n "$kern_event" ]]; then
      printf "  ${DIM}kernel: %s${RST}\n" "$kern_event"
    fi

    case "$phase" in
      need_cable)
        if (( phase_time >= 15 && phase_time < 17 && notified_tier < 1 )); then
          echo
          info "Still waiting... (${elapsed}s)"
          action "Is the USB cable plugged into both the hub and this computer?"
          action "Press the ${BLD}center button${RST}${CYN} (big round one) to power on"
          printf "  ${DIM}Already in DFU (blinking LED)? Just plug USB cable in.${RST}\n"
          notify_user "normal" "LEGO Hub" \
            "Plug USB cable into hub, then press center button"
          notified_tier=1
        elif (( phase_time >= 35 && phase_time < 37 && notified_tier < 2 )); then
          echo
          warn "No hub after ${elapsed}s"
          action "Try a different USB port on this computer"
          action "Make sure the cable is firmly seated in the hub's top port"
          action "Is it a data cable? (some cables are charge-only)"
          hw_diag
          notified_tier=2
        elif (( phase_time >= 50 && phase_time < 52 && notified_tier < 3 )); then
          echo
          warn "No response for ${elapsed}s — showing popup..."
          set +H 2>/dev/null || true
          popup_attention "LEGO Hub — Action Needed" \
            "Plug USB cable into the hub (top port)\nthen press the center button to power on.\n\nAlready in DFU (blinking LED)? Just plug USB cable in.\n\nThe script will detect it automatically." &
          notified_tier=3
        elif (( phase_time >= 70 && phase_time < 72 && notified_tier < 4 )); then
          echo
          info "Tip: the hub is invisible on USB when powered off."
          action "The center button press should be firm — you'll see LEDs light up"
          action "If the hub doesn't turn on, the battery might be low — charge it"
          notified_tier=4
        elif (( phase_time >= 90 && phase_time < 92 && notified_tier < 5 )); then
          echo
          warn "Nothing working? Recovery option (last resort):"
          action "1. Unplug the USB cable"
          action "2. Hold the small ${BLD}Bluetooth button${RST}${CYN} (near USB port)"
          action "3. While holding it, plug USB back in"
          action "4. Wait for pink/purple LED, then release"
          printf "  ${DIM}(this puts the hub in firmware-update mode)${RST}\n"
          notified_tier=5
        elif (( phase_time >= 100 && notified_tier < 6 )); then
          warn "Escalating — repeating alert every 10s"
          start_escalation "LEGO Hub: Plug USB + press center button. Script is waiting."
          notified_tier=6
        fi
        ;;

      need_poweron)
        if (( phase_time >= 10 && phase_time < 12 && notified_tier < 1 )); then
          echo
          info "USB cable is connected but hub isn't responding yet"
          action "Press the ${BLD}center button${RST} (large round button on top)"
          action "You should see the hub's LED matrix light up"
          notify_user "normal" "LEGO Hub" "Press the center button to power on"
          notified_tier=1
        elif (( phase_time >= 20 && phase_time < 22 && notified_tier < 2 )); then
          echo
          warn "Hub still not powering on. The center button press should be firm."
          action "If the hub was recently updated, it may take a few seconds to boot"
          notified_tier=2
        elif (( phase_time >= 35 && notified_tier < 3 )); then
          set +H 2>/dev/null || true
          popup_attention "LEGO Hub — Power On" \
            "Press the large CENTER button on top of the hub.\n\nThe LED matrix should light up." &
          notified_tier=3
        fi
        ;;

      need_shutdown)
        if (( phase_time >= 10 && phase_time < 12 && notified_tier < 1 )); then
          echo
          info "Still waiting for hub to shut down..."
          action "Hold the center button ${BLD}firmly${RST}${CYN} for a full 3 seconds"
          action "All LEDs should turn off"
          notify_user "normal" "LEGO Hub" "Hold center button 3s to shut down"
          notified_tier=1
        elif (( phase_time >= 25 && phase_time < 27 && notified_tier < 2 )); then
          echo
          warn "Hub still running after ${phase_time}s"
          action "Press and hold harder — it needs a full 3-second hold"
          action "Or just unplug the USB cable to cut power"
          notified_tier=2
        elif (( phase_time >= 40 && notified_tier < 3 )); then
          set +H 2>/dev/null || true
          popup_attention "LEGO Hub — Turn Off" \
            "Hold the CENTER button for 3 seconds.\nAll LEDs should go dark.\n\nOr unplug the USB cable." &
          notified_tier=3
        fi
        ;;

      need_usb_from_ble)
        if (( phase_time >= 12 && phase_time < 14 && notified_tier < 1 )); then
          echo
          info "Hub is on Bluetooth but not appearing on USB"
          action "Plug the USB cable into the hub's top port"
          action "Try a different USB cable or port"
          notify_user "normal" "LEGO Hub (BLE)" "Plug USB cable into the hub"
          notified_tier=1
        elif (( phase_time >= 30 && phase_time < 32 && notified_tier < 2 )); then
          echo
          warn "USB still not detected after ${phase_time}s"
          action "Make sure the cable is firmly seated"
          action "Try a different USB port on the computer"
          notified_tier=2
        elif (( phase_time >= 45 && notified_tier < 3 )); then
          set +H 2>/dev/null || true
          popup_attention "LEGO Hub — USB Needed" \
            "Hub is on Bluetooth but USB cable is needed for flashing.\n\nPlug USB cable into the hub's top port." &
          notified_tier=3
        fi
        ;;

      need_dfu_entry)
        if (( phase_time >= 15 && phase_time < 17 && notified_tier < 1 )); then
          echo
          info "Still waiting for firmware-update mode..."
          printf "  ${BLD}Quick check:${RST}\n"
          action "The ${BLD}Bluetooth button${RST}${CYN} is the SMALL one near the USB port"
          action "It's NOT the big center button"
          action "You should see a pink/purple pulsing LED when it works"
          notify_user "normal" "LEGO Hub" "Hold small BT button + replug USB"
          notified_tier=1
        elif (( phase_time >= 35 && phase_time < 37 && notified_tier < 2 )); then
          echo
          warn "Not detected yet. Let's try again from scratch:"
          action "1. Unplug USB cable completely"
          action "2. Wait a few seconds"
          action "3. Press and hold the small Bluetooth button"
          action "4. While holding it, plug USB back in"
          action "5. Keep holding until you see pink/purple LED"
          notified_tier=2
        elif (( phase_time >= 50 && notified_tier < 3 )); then
          set +H 2>/dev/null || true
          popup_attention "LEGO Hub — Firmware Update Mode" \
            "Hold the small BLUETOOTH button (near USB port)\nthen plug USB cable in.\n\nLook for pink/purple pulsing LED." &
          notified_tier=3
        elif (( phase_time >= 80 && notified_tier < 4 )); then
          warn "Escalating — repeating alert every 10s"
          start_escalation "LEGO Hub: Hold BT button + plug USB. Script is waiting."
          notified_tier=4
        fi
        ;;
    esac

    # ── Timeout ──────────────────────────────────────────────
    if (( elapsed >= DFU_WAIT_TIMEOUT )); then
      stop_escalation
      echo
      err "Timed out after ${DFU_WAIT_TIMEOUT}s"
      echo
      printf "  ${BLD}Please try again:${RST}\n"
      action "1. Plug USB cable into the hub"
      action "2. Press center button to power on"
      action "3. Re-run: ${BLD}./dev.sh${RST}"
      echo
      notify_user "critical" "LEGO Dev — Timed Out" \
        "Hub not detected after ${DFU_WAIT_TIMEOUT}s. Re-run ./dev.sh"
      exit 1
    fi

    sleep 1

    # Quiet scanning indicator every 5s (wall-clock time)
    local now_ts
    now_ts=$(date +%s)
    if (( now_ts - last_scan_msg >= 5 )); then
      printf "  ${DIM}[%s] scanning... %ds${RST}\n" "$(ts)" "$elapsed"
      last_scan_msg=$now_ts
    fi

    # ── Fast detection: check for new USB port first ─────────
    if [[ -n "$new_port" ]]; then
      printf "  ${GRN}⚡ New USB device on %s${RST}\n" "$(basename "$new_port")"
      play_sound "$ALERT_SOUND" &
    fi

    # ── Detect: fast USB check every second, BLE only every N seconds ──
    state="$(detect_hub_usb)"
    if [[ "$state" == "none" && $(( now_ts - last_ble_check )) -ge $ble_check_interval ]]; then
      # Slow BLE check (only when USB found nothing)
      local ble_result
      ble_result="$(scan_ble_hub)"
      last_ble_check=$now_ts
      if [[ -n "$ble_result" ]]; then
        state="ble"
      fi
    fi
  done

  stop_escalation
  stop_udev_monitor
  stop_journal_monitor

  # Success celebration — chime + notification
  echo
  ok "Hub in DFU mode — ready to flash!"
  play_sound "$ALERT_SOUND" &
  notify_user "normal" "LEGO Hub Ready" "DFU mode detected — flashing your Rust code now!"
}

# ─── Step 4: FLASH ──────────────────────────────────────────

flash_binary() {
  local dfu_id attempt max_attempts=3

  for attempt in $(seq 1 $max_attempts); do
    dfu_id="$(get_dfu_device_id)"
    if [[ -n "$dfu_id" ]]; then
      break
    fi
    if (( attempt < max_attempts )); then
      warn "DFU device not ready (attempt $attempt/$max_attempts) — waiting 2s..."
      sleep 2
    fi
  done

  if [[ -z "$dfu_id" ]]; then
    err "DFU device disappeared after ${max_attempts} attempts."
    err "Put the hub back in DFU mode and re-run: ./dev.sh"
    exit 1
  fi

  echo
  info "$(bold 'FLASH') — ${BIN_PATH##*/} → ${FLASH_ADDR}  (device: ${dfu_id})"

  local flash_ok=false
  for attempt in $(seq 1 $max_attempts); do
    if dfu-util -d "$dfu_id" -a 0 -s "$FLASH_ADDR" -D "$BIN_PATH" 2>&1; then
      flash_ok=true
      break
    fi
    if (( attempt == 1 )); then
      # Permission denied? Try with sudo
      warn "dfu-util failed, retrying with sudo..."
      if sudo dfu-util -d "$dfu_id" -a 0 -s "$FLASH_ADDR" -D "$BIN_PATH" 2>&1; then
        flash_ok=true
        break
      fi
    fi
    if (( attempt < max_attempts )); then
      warn "Flash attempt $attempt failed — waiting 2s before retry..."
      sleep 2
      # Re-check device ID (hub might have re-enumerated)
      dfu_id="$(get_dfu_device_id)"
      if [[ -z "$dfu_id" ]]; then
        err "DFU device gone during flash retry."
        exit 1
      fi
    fi
  done

  if [[ "$flash_ok" != true ]]; then
    err "Flash failed after $max_attempts attempts."
    exit 1
  fi

  ok "Flashed"
}

# ─── Step 5: RESET & RUN ────────────────────────────────────

reset_and_run() {
  local dfu_id
  dfu_id="$(get_dfu_device_id)"
  if [[ -z "$dfu_id" ]]; then
    warn "Can't send reset — DFU device gone. Unplug USB + press center button to boot."
    return 0
  fi

  info "Resetting hub..."
  # Use dfu-util to issue a USB reset which causes the bootloader to jump to the app
  # The :leave suffix triggers DFU_DNLOAD with zero length + USB reset
  dfu-util -d "$dfu_id" -a 0 -s "${FLASH_ADDR}:leave" -R -D /dev/null 2>/dev/null || \
    sudo dfu-util -d "$dfu_id" -a 0 -s "${FLASH_ADDR}:leave" -R -D /dev/null 2>/dev/null || true

  # Give hub a moment to reboot
  sleep 2

  # Check what DFU mode we were in for contextual help
  if [[ "$dfu_id" == "${ST_VID}:${ST_DFU_PID}" ]]; then
    ok "Hub rebooting — LEGO bootloader → your Rust code"
    echo "  When USB is connected, your code will auto-enter DFU after one demo cycle."
    echo "  Next time: just run ${BLD}./dev.sh${RST} — no manual DFU needed!"
  else
    ok "Hub should be running your code now"
    echo "  With USB connected, it will auto-enter DFU after one demo cycle."
    echo "  Next time: just run ${BLD}./dev.sh${RST} — no manual DFU needed!"
  fi
}

# ─── MAIN ────────────────────────────────────────────────────

# ─── Source file checksum (for watch mode) ─────────────────
src_checksum() {
  find "$CRATE_PATH/src" -name '*.rs' -exec md5sum {} + 2>/dev/null | sort | md5sum | cut -d' ' -f1
}

# ─── Bootloader build + bootstrap ────────────────────────────

build_bootloader() {
  info "$(bold 'COMPILE') — bootloader"
  if ! ( cd "$ROOT_DIR/bootloader" && cargo build --release 2>&1 ); then
    err "Bootloader compilation failed."
    exit 1
  fi
  ok "Bootloader compiled"

  local bl_elf="$ROOT_DIR/target/thumbv7em-none-eabihf/release/bootloader"
  BOOTLOADER_BIN="${bl_elf}.bin"
  arm-none-eabi-objcopy -O binary "$bl_elf" "$BOOTLOADER_BIN"
  local size
  size=$(stat -c%s "$BOOTLOADER_BIN" 2>/dev/null || stat -f%z "$BOOTLOADER_BIN")
  ok "Bootloader binary: ${size} bytes"
}

# Flash bootloader to 0x08008000 via LEGO DFU, then wait for STM32 DFU
flash_bootloader_to_hub() {
  local dfu_id
  dfu_id="$(get_dfu_device_id)"

  if [[ -z "$dfu_id" ]]; then
    err "No DFU device found."
    exit 1
  fi

  if [[ "$dfu_id" == "${ST_VID}:${ST_DFU_PID}" ]]; then
    warn "Hub is in STM32 system DFU — bootloader is already installed."
    return 0
  fi

  echo
  info "$(bold 'FLASH BOOTLOADER') — bootloader.bin → ${BOOTLOADER_FLASH_ADDR}  (device: ${dfu_id})"

  if ! dfu-util -d "$dfu_id" -a 0 -s "$BOOTLOADER_FLASH_ADDR" -D "$BOOTLOADER_BIN" 2>&1; then
    if ! sudo dfu-util -d "$dfu_id" -a 0 -s "$BOOTLOADER_FLASH_ADDR" -D "$BOOTLOADER_BIN" 2>&1; then
      err "Failed to flash bootloader."
      exit 1
    fi
  fi
  ok "Bootloader flashed"

  info "Resetting hub — bootloader will enter STM32 system DFU (no valid app)..."
  dfu-util -d "$dfu_id" -a 0 -s "${BOOTLOADER_FLASH_ADDR}:leave" -R -D /dev/null 2>/dev/null || \
    sudo dfu-util -d "$dfu_id" -a 0 -s "${BOOTLOADER_FLASH_ADDR}:leave" -R -D /dev/null 2>/dev/null || true

  # Wait for hub to reappear as STM32 DFU (bootloader found no valid app → system DFU)
  info "Waiting for STM32 system DFU (0483:df11)..."
  local wait_count=0
  while true; do
    sleep 1
    wait_count=$((wait_count + 1))
    local new_id
    new_id="$(get_dfu_device_id)"
    if [[ "$new_id" == "${ST_VID}:${ST_DFU_PID}" ]]; then
      ok "Hub in STM32 system DFU — ready for app flash"
      return 0
    fi
    if (( wait_count > 30 )); then
      warn "Timeout waiting for STM32 DFU."
      warn "Try: unplug USB, press center button to boot, replug USB."
      return 0
    fi
  done
}

# Smart flash: detect DFU device type → use correct address
smart_flash() {
  local dfu_id
  dfu_id="$(get_dfu_device_id)"

  if [[ -z "$dfu_id" ]]; then
    err "No DFU device found after wait."
    exit 1
  fi

  if [[ "$dfu_id" == "${ST_VID}:${ST_DFU_PID}" ]]; then
    # STM32 system DFU → flash app to 0x08010000
    FLASH_ADDR="$APP_FLASH_ADDR"
    info "$(bold 'FLASH APP') — ${BIN_PATH##*/} → ${FLASH_ADDR}  (device: ${dfu_id})"
  else
    # LEGO DFU → need to install bootloader first, then flash app
    warn "Hub in LEGO DFU — bootloader not installed yet."
    info "Auto-installing bootloader first..."
    build_bootloader
    flash_bootloader_to_hub

    # Re-check DFU device
    dfu_id="$(get_dfu_device_id)"
    if [[ -z "$dfu_id" ]]; then
      err "Lost DFU device after bootloader flash."
      exit 1
    fi
    FLASH_ADDR="$APP_FLASH_ADDR"
    info "$(bold 'FLASH APP') — ${BIN_PATH##*/} → ${FLASH_ADDR}  (device: ${dfu_id})"
  fi

  # Flash the app binary
  local flash_ok=false
  local max_attempts=3
  for attempt in $(seq 1 $max_attempts); do
    if dfu-util -d "$dfu_id" -a 0 -s "$FLASH_ADDR" -D "$BIN_PATH" 2>&1; then
      flash_ok=true
      break
    fi
    if (( attempt == 1 )); then
      warn "dfu-util failed, retrying with sudo..."
      if sudo dfu-util -d "$dfu_id" -a 0 -s "$FLASH_ADDR" -D "$BIN_PATH" 2>&1; then
        flash_ok=true
        break
      fi
    fi
    if (( attempt < max_attempts )); then
      warn "Flash attempt $attempt failed — waiting 2s..."
      sleep 2
      dfu_id="$(get_dfu_device_id)"
    fi
  done

  if [[ "$flash_ok" != "true" ]]; then
    err "Flash failed after $max_attempts attempts."
    exit 1
  fi
  ok "Flashed"
}

smart_reset_and_run() {
  local dfu_id
  dfu_id="$(get_dfu_device_id)"
  if [[ -z "$dfu_id" ]]; then
    warn "DFU device gone. Unplug USB + press center button to boot."
    return 0
  fi

  info "Resetting hub..."
  dfu-util -d "$dfu_id" -a 0 -s "${FLASH_ADDR}:leave" -R -D /dev/null 2>/dev/null || \
    sudo dfu-util -d "$dfu_id" -a 0 -s "${FLASH_ADDR}:leave" -R -D /dev/null 2>/dev/null || true
  sleep 2
  ok "Hub should be running — LEGO BL → Rust BL → your app"
}

# ─── Watch mode: auto-rebuild+flash on code changes ─────────
watch_loop() {
  local crate_name
  crate_name="$(basename "$CRATE_DIR")"
  local iteration=0
  local last_hash=""

  printf "\n${BLD}═══ LEGO Rust Watch Mode ═══${RST}  ${DIM}%s${RST}\n" "$crate_name"
  echo "  Watching ${CYN}${CRATE_DIR}/src/${RST} for changes."
  echo "  Edit code, save → auto-build → auto-flash → hub runs new code."
  echo "  Press ${BLD}Ctrl+C${RST} to stop."
  echo

  validate_setup

  # Initial build + flash
  iteration=$((iteration + 1))
  printf "${BLD}── iteration %d ──${RST}\n" "$iteration"
  build_rust
  make_binary
  last_hash="$(src_checksum)"
  wait_for_dfu
  smart_flash
  smart_reset_and_run
  echo
  ok "Hub running. Watching for code changes..."
  echo

  # Watch loop
  while true; do
    sleep 1
    local new_hash
    new_hash="$(src_checksum)"
    if [[ "$new_hash" != "$last_hash" ]]; then
      echo
      printf "${YEL}[%s]${RST} ${BLD}Source changed — rebuilding...${RST}\n" "$(ts)"
      play_sound "$ALERT_SOUND" &
      iteration=$((iteration + 1))
      printf "${BLD}── iteration %d ──${RST}\n" "$iteration"
      build_rust
      make_binary
      last_hash="$(src_checksum)"

      # Wait for hub to re-enter DFU (dev-mode firmware does this automatically)
      info "Waiting for hub to re-enter DFU..."
      local dfu_wait=0
      while [[ "$(detect_hub_usb)" != "dfu" ]]; do
        sleep 1
        dfu_wait=$((dfu_wait + 1))
        if (( dfu_wait > DFU_WAIT_TIMEOUT )); then
          err "Hub did not re-enter DFU within ${DFU_WAIT_TIMEOUT}s."
          info "Waiting for manual DFU... (hold BT button + plug USB)"
          wait_for_dfu
          break
        fi
      done
      if [[ "$(detect_hub_usb)" == "dfu" ]]; then
        ok "Hub in DFU mode"
      fi

      smart_flash
      smart_reset_and_run
      echo
      ok "Hub running. Watching for code changes..."
      echo
    fi
  done
}

main() {
  local crate_name
  crate_name="$(basename "$CRATE_DIR")"

  printf "\n${BLD}═══ LEGO Rust Dev Cycle ═══${RST}  ${DIM}%s${RST}\n" "$crate_name"

  validate_setup
  build_rust
  make_binary
  wait_for_dfu
  smart_flash
  smart_reset_and_run

  echo
  printf "${GRN}${BLD}═══ DONE ═══${RST}\n"
  echo "  Hub boot chain: LEGO BL → Rust bootloader → your app"
  echo "  App auto-enters DFU after dev demo (USB connected)."
  echo "  To iterate: edit code → ./dev.sh"
  echo
}

bootloader_main() {
  printf "\n${BLD}═══ LEGO Rust Bootloader Flash ═══${RST}\n"
  echo "  Flashing Rust bootloader to 0x08008000 via LEGO DFU."
  echo "  This is a ONE-TIME operation."
  echo

  validate_setup
  build_bootloader
  wait_for_dfu
  flash_bootloader_to_hub

  echo
  printf "${GRN}${BLD}═══ DONE ═══${RST}\n"
  echo "  Bootloader installed at 0x08008000."
  echo "  Hub is in STM32 system DFU (0483:df11)."
  echo "  Now run: ./dev.sh    to flash your app."
  echo
}

# ─── Monitor build + flash ───────────────────────────────────

build_monitor() {
  info "$(bold 'COMPILE') — monitor"
  if ! ( cd "$ROOT_DIR/monitor" && cargo build --release 2>&1 ); then
    err "Monitor compilation failed."
    exit 1
  fi
  ok "Monitor compiled"

  local mon_elf="$ROOT_DIR/target/thumbv7em-none-eabihf/release/monitor"
  MONITOR_BIN="${mon_elf}.bin"
  arm-none-eabi-objcopy -O binary "$mon_elf" "$MONITOR_BIN"
  local size
  size=$(stat -c%s "$MONITOR_BIN" 2>/dev/null || stat -f%z "$MONITOR_BIN")
  ok "Monitor binary: ${size} bytes"
}

flash_monitor_to_hub() {
  local dfu_id
  dfu_id="$(get_dfu_device_id)"

  if [[ -z "$dfu_id" ]]; then
    err "No DFU device found."
    exit 1
  fi

  if [[ "$dfu_id" == "${ST_VID}:${ST_DFU_PID}" ]]; then
    # STM32 system DFU — flash monitor to 0x08008000
    info "$(bold 'FLASH MONITOR') — monitor.bin → ${BOOTLOADER_FLASH_ADDR}  (via STM32 DFU)"
    if ! dfu-util -d "$dfu_id" -a 0 -s "${BOOTLOADER_FLASH_ADDR}:leave" -D "$MONITOR_BIN" 2>&1; then
      if ! sudo dfu-util -d "$dfu_id" -a 0 -s "${BOOTLOADER_FLASH_ADDR}:leave" -D "$MONITOR_BIN" 2>&1; then
        err "Failed to flash monitor."
        exit 1
      fi
    fi
  else
    # LEGO DFU — flash directly
    info "$(bold 'FLASH MONITOR') — monitor.bin → ${BOOTLOADER_FLASH_ADDR}  (via LEGO DFU)"
    if ! dfu-util -d "$dfu_id" -a 0 -s "$BOOTLOADER_FLASH_ADDR" -D "$MONITOR_BIN" 2>&1; then
      if ! sudo dfu-util -d "$dfu_id" -a 0 -s "$BOOTLOADER_FLASH_ADDR" -D "$MONITOR_BIN" 2>&1; then
        err "Failed to flash monitor."
        exit 1
      fi
    fi
  fi
  ok "Monitor flashed"
}

monitor_main() {
  printf "\n${BLD}═══ LEGO Hub Debug Monitor Flash ═══${RST}\n"
  echo "  Flashing debug monitor to 0x08008000."
  echo "  Replaces the simple bootloader with USB serial monitor."
  echo

  validate_setup
  build_monitor
  wait_for_dfu
  flash_monitor_to_hub

  echo
  printf "${GRN}${BLD}═══ DONE ═══${RST}\n"
  echo "  Monitor installed at 0x08008000."
  echo "  Connect USB and open serial terminal to interact."
  echo "  Try: picocom /dev/ttyACM0  or  minicom -D /dev/ttyACM0"
  echo
}

if [[ "$BOOTLOADER_MODE" -eq 1 ]]; then
  bootloader_main
elif [[ "$MONITOR_MODE" -eq 1 ]]; then
  monitor_main
elif [[ "$WATCH_MODE" -eq 1 ]]; then
  watch_loop
else
  main
fi
