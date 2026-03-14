#!/usr/bin/env python3
"""Upload a binary to LEGO Hub via monitor's 'upload' command.

Usage: python3 upload.py [binary] [port]
  binary: path to .bin file (default: /tmp/led-test.bin)
  port:   serial port (default: /dev/ttyACM1)

The script sends 'upload' to the monitor, which erases sector 4
(64KB at 0x08010000), then sends each word as "ADDR VALUE" hex lines.
Finishes with 'end', then optionally sends 'run'.
"""
import serial
import sys
import time

BIN  = sys.argv[1] if len(sys.argv) > 1 else '/tmp/led-test.bin'
PORT = sys.argv[2] if len(sys.argv) > 2 else '/dev/ttyACM1'
BASE = 0x08010000

with open(BIN, 'rb') as f:
    data = f.read()

# Pad to word boundary
if len(data) % 4:
    data += b'\xff' * (4 - len(data) % 4)

words = len(data) // 4
print(f"Binary: {BIN} ({len(data)} bytes, {words} words)")
print(f"Target: {BASE:#010x}")
print(f"Port:   {PORT}")

ser = serial.Serial(PORT, 115200, timeout=2)
time.sleep(0.3)
ser.reset_input_buffer()

# Drain any pending prompt
ser.write(b'\r\n')
time.sleep(0.3)
ser.reset_input_buffer()

# Send upload command
ser.write(b'upload\r\n')
time.sleep(2.0)  # wait for sector erase
resp = ser.read(ser.in_waiting or 1)
print(resp.decode(errors='replace'), end='')

# Verify we got the expected response
if b'end' not in resp and b'Send' not in resp:
    print("\nERROR: Monitor did not acknowledge 'upload' command.")
    print("       Got:", repr(resp))
    ser.close()
    sys.exit(1)

# Send data word by word
t0 = time.time()
for i in range(0, len(data), 4):
    word = int.from_bytes(data[i:i+4], 'little')
    addr = BASE + i
    ser.write(f'{addr:08X} {word:08X}\r\n'.encode())
    if (i // 4) % 128 == 0:
        print(f'\r  {i}/{len(data)} bytes ...', end='', flush=True)
        # Small drain to keep buffers happy
        if ser.in_waiting:
            ser.read(ser.in_waiting)

# Finish
ser.write(b'end\r\n')
time.sleep(0.3)
resp = ser.read(ser.in_waiting or 1)
elapsed = time.time() - t0
print(f'\r  {len(data)}/{len(data)} bytes in {elapsed:.1f}s')
print(resp.decode(errors='replace'))

ans = input("Run app? [y/N] ").strip().lower()
if ans == 'y':
    ser.write(b'run\r\n')
    time.sleep(0.3)
    print(ser.read(ser.in_waiting or 1).decode(errors='replace'))

ser.close()
