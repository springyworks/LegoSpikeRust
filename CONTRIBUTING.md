# Contributing to LegoSpikeRust

Thanks for your interest in contributing to bare-metal Rust firmware for LEGO SPIKE Prime!

This is a complex embedded systems project — direct register manipulation on a Cortex-M4F, no OS, no HAL. Contributions are welcome, but please read this guide first.

## Use an AI Coding Assistant

**Seriously.** This project involves bare-metal register-level programming on an STM32F413 with no HAL crate. You're writing directly to peripheral registers (RCC, GPIO, SPI, TIM, ADC, USB OTG, FPB, DWT) and dealing with ARM exception handling, linker scripts, and memory-mapped I/O.

An AI coding assistant (GitHub Copilot, Cursor, etc.) is extremely helpful for:
- Looking up STM32F413 register bit fields and addresses
- Understanding ARM Cortex-M4 exception model (DebugMonitor, SysTick, etc.)
- Navigating the TLC5955 datasheet for LED driver programming
- Generating correct inline assembly for exception trampolines
- Reverse-engineering hardware from the pybricks C source

We built this entire project with the help of AI coding assistants and it made the difference between "impossible side project" and "working firmware."

## How to Contribute

### 1. Fork & Clone

```bash
git clone https://github.com/YOUR_USERNAME/LegoSpikeRust.git
cd LegoSpikeRust
```

### 2. Set Up Toolchain

```bash
# Rust stable + Cortex-M4F target
rustup target add thumbv7em-none-eabihf

# ARM toolchain (for objcopy)
sudo apt install gcc-arm-none-eabi

# DFU flasher + serial tools
sudo apt install dfu-util picocom
pip install pyserial
```

### 3. Get Pybricks Source (Reference Only)

The pybricks-micropython source tree is used as a hardware reference (not compiled/linked). Clone it alongside:

```bash
git clone https://github.com/pybricks/pybricks-micropython.git
```

The key reference files are in `lib/pbio/platform/prime_hub/` and `lib/pbio/drv/`.

### 4. Build & Test

```bash
# Build the monitor
cd monitor && cargo build --release

# Build the LED test app
cd led-test && cargo build --release

# Flash monitor (one-time DFU)
./dev.sh --monitor

# Upload app (serial, no DFU needed)
python3 upload.py led-test.bin /dev/ttyACM0
```

### 5. Make Your Changes

Create a feature branch:
```bash
git checkout -b feature/your-feature
```

### 6. Submit a Pull Request

- Write a clear description of what you changed and why
- If you're adding peripheral support, mention which STM32F413 registers you're accessing
- Include any hardware-specific gotchas you discovered (like the TLC5955 double-latch or the ADC resistor ladder)

## What We'd Love Help With

Check the [project status in README.md](README.md#project-status) for the current TODO list. Priority areas:

- **Motor encoder reading** — quadrature decoding via TIM2/TIM3/TIM4
- **PID motor control** — porting concepts from pybricks' `pbio/control.c`
- **IMU driver** — LSM6DS3TR-C accelerometer + gyroscope on I2C/SPI
- **Bluetooth LE** — the hub has a BT radio, completely untouched
- **`cont` command fix** — the continue-from-breakpoint still re-triggers immediately (SysTick re-pends MON_PEND)
- **More app examples** — sensor reading, motor demos, light shows

## Hardware You'll Need

- **LEGO SPIKE Prime Hub** (set 45678) or **LEGO Mindstorms Robot Inventor** (set 51515) — same hardware
- **USB-A to Micro-USB cable** — for flashing and serial communication
- Optional: LEGO motors, sensors for testing I/O features

## Code Style

- `#![no_std]`, `#![no_main]` — this is bare metal
- Direct register access via `core::ptr::read_volatile` / `write_volatile`
- Constants for register addresses at the top of each file
- Inline assembly only where Rust can't express it (exception handlers, trampolines)
- Comments explaining register bit fields and why specific values are used
- Test on real hardware when possible (QEMU doesn't emulate LEGO hub peripherals)

## Questions?

Open an issue. We're happy to help newcomers get started with embedded Rust.
