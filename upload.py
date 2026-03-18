#!/usr/bin/env python3
"""Upload a binary to LEGO Hub via monitor's 'upload' command.

Usage: python3 upload.py [binary] [port] [--run]
  binary: path to .bin file (default: /tmp/led-test.bin)
  port:   serial port (auto-detected, or /dev/ttyACM0)
  --run:  auto-run after upload (no prompt)

Handles multiple hub states:
  - Monitor idle (prompt '> ')   → send 'upload' directly
  - App running                  → unplug/replug needed (USB CDC is app's)
  - No serial port               → wait for hub to appear
  - Port busy                    → kill existing users, retry
"""
import serial
import subprocess
import sys
import os
import glob
import time

# ── Args ──
BIN = None
PORT = None
AUTO_RUN = False
for arg in sys.argv[1:]:
    if arg == '--run':
        AUTO_RUN = True
    elif BIN is None and not arg.startswith('/dev/'):
        BIN = arg
    elif PORT is None:
        PORT = arg

BIN = BIN or '/tmp/led-test.bin'
BASE = 0x08010000

# ── Find binary ──
if not os.path.isfile(BIN):
    print(f"ERROR: Binary not found: {BIN}")
    sys.exit(1)

with open(BIN, 'rb') as f:
    data = f.read()

if len(data) % 4:
    data += b'\xff' * (4 - len(data) % 4)

words = len(data) // 4
print(f"Binary: {BIN} ({len(data)} bytes, {words} words)")
print(f"Target: {BASE:#010x}")

# ── Auto-detect serial port ──
def find_monitor_port():
    """Find the monitor's ttyACM port by checking USB VID:PID 1209:0001."""
    for devpath in sorted(glob.glob('/sys/bus/usb/devices/*/idVendor')):
        try:
            vid = open(devpath).read().strip()
            pid = open(devpath.replace('idVendor', 'idProduct')).read().strip()
            if vid == '1209' and pid == '0001':
                # Find the tty device under this USB device
                usb_dir = os.path.dirname(devpath)
                for root, dirs, files in os.walk(usb_dir):
                    for d in dirs:
                        if d.startswith('ttyACM'):
                            return f'/dev/{d}'
        except (IOError, OSError):
            continue
    # Fallback: first ttyACM
    ports = sorted(glob.glob('/dev/ttyACM*'))
    return ports[0] if ports else None

if PORT is None:
    PORT = find_monitor_port()
    if PORT is None:
        print("No serial port found. Is the hub plugged in with monitor running?")
        print("Waiting for hub...")
        for i in range(30):
            time.sleep(1)
            PORT = find_monitor_port()
            if PORT:
                print(f"  Found: {PORT}")
                break
        if PORT is None:
            print("ERROR: No hub detected after 30s. Check USB cable.")
            sys.exit(1)

print(f"Port:   {PORT}")

# ── Kill existing port users ──
subprocess.run(['fuser', '-k', PORT], stderr=subprocess.DEVNULL, stdout=subprocess.DEVNULL)
time.sleep(0.3)

# ── Open serial with retries ──
ser = None
for attempt in range(5):
    try:
        ser = serial.Serial(PORT, 115200, timeout=2, write_timeout=5)
        break
    except serial.SerialException as e:
        if attempt < 4:
            print(f"  Port busy, retrying ({attempt+1}/5)...")
            time.sleep(1)
        else:
            print(f"ERROR: Cannot open {PORT}: {e}")
            sys.exit(1)

time.sleep(0.3)
ser.reset_input_buffer()

# ── Wake up monitor: send a few newlines, look for '>' prompt ──
def drain():
    """Read everything available."""
    time.sleep(0.1)
    buf = b''
    while ser.in_waiting:
        buf += ser.read(ser.in_waiting)
        time.sleep(0.05)
    return buf

def safe_write(data):
    """Write with timeout recovery — reopen port if needed."""
    global ser
    try:
        ser.write(data)
    except serial.SerialTimeoutException:
        print("  Write timeout — resetting port...")
        try:
            ser.close()
        except Exception:
            pass
        time.sleep(1)
        ser = serial.Serial(PORT, 115200, timeout=2, write_timeout=5)
        time.sleep(0.3)
        ser.reset_input_buffer()
        ser.write(data)

# Send newlines to get a prompt — handles monitor being in various states
for _ in range(3):
    safe_write(b'\r\n')
    time.sleep(0.2)
got = drain()

# If we see '>' we're at the monitor prompt — good
if b'>' not in got:
    # Maybe app is running — try sending more newlines
    print("  Waking monitor...")
    for _ in range(3):
        ser.write(b'\r\n')
        time.sleep(0.5)
    got = drain()

print(f"  Monitor response: {got[-60:].decode(errors='replace').strip()}")

# ── Send upload command ──
ser.reset_input_buffer()
ser.write(b'upload\r\n')

# Wait for "Send:" which means erase completed and monitor is ready for data.
# The full response is: "Erasing sector 4... OK\r\nSend: ADDR VALUE (hex), 'end' to finish.\r\n"
# We look for "Send:" specifically (not "Send" or "end" which match too early).
buf = b''
deadline = time.time() + 30.0  # 30s — erase can be slow
while time.time() < deadline:
    n = ser.in_waiting
    if n:
        buf += ser.read(n)
        if b'Send:' in buf:
            break
    time.sleep(0.2)

print(buf.decode(errors='replace').strip())

if b'Send:' not in buf:
    # Maybe it already erased from a previous aborted upload — try sending a
    # test line and see if it accepts data
    if b'Erasing' in buf:
        print("  Erase in progress, waiting longer...")
        extra_deadline = time.time() + 20.0
        while time.time() < extra_deadline:
            n = ser.in_waiting
            if n:
                buf += ser.read(n)
                if b'Send:' in buf:
                    break
            time.sleep(0.2)

    if b'Send:' not in buf:
        print(f"\nERROR: Monitor did not reach 'Send:' state.")
        print(f"       Got: {repr(buf[-200:])}")
        ser.close()
        sys.exit(1)

# ── Send data word by word ──
t0 = time.time()
for i in range(0, len(data), 4):
    word = int.from_bytes(data[i:i+4], 'little')
    addr = BASE + i
    line = f'{addr:08X} {word:08X}\r\n'.encode()
    ser.write(line)
    if (i // 4) % 128 == 0:
        print(f'\r  {i}/{len(data)} bytes ...', end='', flush=True)
        if ser.in_waiting:
            ser.read(ser.in_waiting)

# ── Finish ──
ser.write(b'end\r\n')
time.sleep(0.5)
resp = b''
deadline = time.time() + 3.0
while time.time() < deadline:
    if ser.in_waiting:
        resp += ser.read(ser.in_waiting)
    time.sleep(0.1)
elapsed = time.time() - t0
print(f'\r  {len(data)}/{len(data)} bytes in {elapsed:.1f}s')
print(resp.decode(errors='replace').strip())

# ── Run ──
if AUTO_RUN:
    ans = 'y'
else:
    try:
        ans = input("Run app? [y/N] ").strip().lower()
    except EOFError:
        ans = 'y'

if ans == 'y':
    ser.write(b'run\r\n')
    time.sleep(0.5)
    out = b''
    while ser.in_waiting:
        out += ser.read(ser.in_waiting)
        time.sleep(0.1)
    print(out.decode(errors='replace').strip())

ser.close()
