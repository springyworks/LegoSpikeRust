# LegoSpikeRust

**Bare-metal Rust firmware for the LEGO SPIKE Prime / Mindstorms Robot Inventor Hub (51515)**

No OS. No HAL crate. No RTOS. Just Rust, direct register access, and a resident debug monitor — running on a Cortex-M4F at 96 MHz.

## What This Is

A from-scratch Rust firmware stack for the LEGO Education SPIKE Prime Hub (STM32F413VGT6), built entirely without an operating system. The only "OS" here is a 25 KB Rust debug monitor that lives in flash and supervises application code.

### Architecture

```
┌─────────────────────────────────────┐
│  Flash Layout (1 MB)                │
├─────────────┬───────────────────────┤
│ 0x08000000  │ LEGO DFU Bootloader   │ 32 KB (factory, untouched)
│ 0x08008000  │ Rust Debug Monitor    │ 32 KB (our code)
│ 0x08010000  │ Application Firmware  │ 960 KB (our code)
└─────────────┴───────────────────────┘

┌─────────────────────────────────────┐
│  RAM Layout (320 KB)                │
├─────────────┬───────────────────────┤
│ 0x20000000  │ App RAM               │ 312 KB
│ 0x2004E000  │ Monitor RAM           │ 8 KB (persists across app runs)
│ 0x2004FFE0  │ Trampolines           │ DebugMonitor + SysTick handler ptrs
│ 0x2004FFF0  │ DFU Magic             │ 0xDEADB007 → reboot to DFU
└─────────────┴───────────────────────┘
```

### Boot Chain

1. **LEGO DFU Bootloader** (factory) → validates and jumps to...
2. **Rust Debug Monitor** (0x08008000) → USB CDC serial shell, or jumps to...
3. **Application** (0x08010000) → user firmware with trampoline hooks back to monitor

## Features

### Debug Monitor (`monitor/` — 1,585 lines of Rust, 25 KB binary)

A resident embedded debug monitor providing:

- **USB CDC serial shell** — interactive command line over USB
- **Memory inspection** — `peek`, `poke`, `dump` (hex dump with ASCII)
- **Hardware breakpoints** — FPB (Flash Patch and Breakpoint unit), up to 6 breakpoints
- **Data watchpoints** — DWT (Data Watchpoint and Trace unit), up to 4 watchpoints
- **Single-step execution** — ARM DebugMonitor exception (MON_STEP)
- **Register inspection** — `regs`, `set r0 value` while halted at breakpoint
- **Center button pause** — press the hub's center button to pause a running app (via ADC resistor ladder reading + SysTick interrupt)
- **Serial app upload** — flash new application code without DFU mode (`upload` command + `upload.py`)
- **Motor diagnostics** — `motors` command dumps GPIO/TIM1 config for all 6 ports
- **DFU entry** — `dfu` command to enter STM32 system bootloader

### Application Trampolines

Apps link against the monitor via RAM-based function pointers:
- `0x2004FFE0` — DebugMonitor handler (breakpoints, single-step)
- `0x2004FFE4` — SysTick handler (center button polling via ADC)

Apps are simple `#[no_std]` binaries with inline assembly trampolines that forward exceptions to the monitor.

### Workspace Crates

| Crate | Purpose | Binary Size |
|-------|---------|-------------|
| `monitor/` | Resident debug monitor + USB serial shell | 25 KB |
| `led-test/` | LED matrix test app (TLC5955 driver) | 3.3 KB |
| `hub-motors/` | Motor control with RTIC v2 + defmt | — |
| `bootloader/` | Simple bootloader (superseded by monitor) | — |

## Hardware

- **MCU**: STM32F413VGT6 — ARM Cortex-M4F, 96 MHz, 1 MB Flash, 320 KB RAM
- **LED Matrix**: TLC5955 48-channel PWM driver on SPI1 (5×5 RGB matrix + status LEDs)
- **Motors**: 6 ports (A–F), H-bridge via TIM1/TIM4 PWM, quadrature encoders
- **IMU**: LSM6DS3TR-C (accelerometer + gyroscope)
- **Center Button**: Resistor ladder on PC4 (ADC1 channel 14) — not a simple GPIO!
- **USB**: OTG FS (PA11/PA12), CDC serial for monitor communication
- **Power**: PA13 = power hold (must set HIGH immediately on boot)

## Acknowledgments — Pybricks

This project heavily references and reuses knowledge from the **[Pybricks](https://github.com/pybricks/pybricks-micropython)** project. The Pybricks C source code (`lib/pbio/`) was invaluable for reverse-engineering the SPIKE Prime hardware:

- **Resistor ladder button driver** (`drv/button/button_resistor_ladder.c`) — discovered that the center button uses an ADC-based resistor ladder, not simple GPIO. Our SysTick handler does a single-shot ADC conversion on channel 14 to detect button presses.
- **TLC5955 LED driver** (`drv/pwm/pwm_tlc5955_stm32.c`) — learned the control register must be sent twice, the channel-to-LED mapping, and the SPI/LAT/GSCLK pin configuration.
- **Motor port GPIO mapping** — port pin assignments (ENA/ENB/IN1/IN2), encoder timer channels, H-bridge topology.
- **Platform configuration** (`lib/pbio/platform/prime_hub/`) — clock tree (PLL settings for 96 MHz + 48 MHz USB), GPIO alternate functions, peripheral addresses.
- **USB OTG configuration** — FIFO depths, endpoint counts, VBUS detection via PA9.

The Pybricks source tree is included as a reference in `pybricks-micropython/`. No Pybricks C code is compiled or linked — all firmware is written in Rust, but the hardware knowledge came from studying their excellent codebase.

## Build & Deploy

### Prerequisites

```bash
# Rust toolchain (stable, with Cortex-M4F target)
rustup target add thumbv7em-none-eabihf

# ARM toolchain (for objcopy)
sudo apt install gcc-arm-none-eabi

# DFU flasher
sudo apt install dfu-util

# Python (for upload.py)
pip install pyserial
```

### Flash the Monitor (one-time, requires DFU)

```bash
cd monitor && cargo build --release
arm-none-eabi-objcopy -O binary target/thumbv7em-none-eabihf/release/monitor /tmp/monitor.bin
# Put hub in DFU mode (hold center button while plugging USB)
dfu-util -d 0694:0011 -a 0 -s 0x08008000:leave -D /tmp/monitor.bin
```

Or use the dev script:
```bash
./dev.sh --monitor
```

### Upload Applications (no DFU needed!)

Once the monitor is flashed, apps are uploaded over serial:

```bash
cd led-test && cargo build --release
arm-none-eabi-objcopy -O binary target/thumbv7em-none-eabihf/release/led-test led-test.bin
python3 upload.py led-test.bin /dev/ttyACM0
```

### Monitor Commands

```
> help              — show all commands
> upload            — flash app binary over serial
> run               — arm monitor + jump to app (center button = pause)
> motors            — dump motor GPIO/TIM1 config
> peek <addr>       — read 32-bit word
> poke <addr> <val> — write 32-bit word
> dump <addr> <len> — hex dump
> bp <addr>         — set hardware breakpoint
> watch <addr>      — set data watchpoint
> dbgmon            — enable DebugMonitor exception
> dfu               — enter STM32 system DFU
> reboot            — reset MCU

At breakpoint (dbg> prompt):
> cont              — continue execution
> step              — single-step one instruction
> regs              — show saved registers
> set <reg> <val>   — modify register
> stop              — stop app, reboot to monitor
```

## Technical Details

### No OS — Just Interrupts and Bare Metal

There is no RTOS, no scheduler, no threads. The monitor is a single `#[no_main]` binary that:
1. Configures the PLL (96 MHz HCLK, 48 MHz USB)
2. Initializes USB OTG FS as CDC serial
3. Runs a polling loop reading serial commands
4. Uses ARM DebugMonitor exception (not halt-mode debug) for breakpoints and single-step
5. Uses SysTick interrupt at 100 Hz to poll the center button via ADC

### Center Button — The Resistor Ladder Trap

The SPIKE Prime center button is **not** a simple GPIO digital input. It's part of a resistor ladder (multiple buttons sharing one analog ADC pin, PC4). When pressed, the voltage drops from ~3.3V to ~2.5V — still above the digital GPIO threshold of ~1.65V. You **must** use ADC to detect it. This took considerable reverse-engineering of the Pybricks source to discover.

### TLC5955 — The Double-Latch Gotcha

The TLC5955 LED driver requires the control register to be sent **twice** before grayscale data produces visible output. This is per the TLC5955 datasheet but easy to miss. The control register sets dot correction, max current, brightness, and operating modes.

## Project Status

- [x] Custom Rust bootloader with DFU support
- [x] Resident debug monitor with USB serial shell
- [x] Memory inspection (peek/poke/dump)
- [x] Hardware breakpoints (FPB) and data watchpoints (DWT)
- [x] Single-step debugging via DebugMonitor exception
- [x] Center button pause (ADC resistor ladder)
- [x] Serial app upload (no DFU for app dev)
- [x] TLC5955 LED matrix driver (5×5 + status LEDs)
- [x] Motor PWM control (TIM1/TIM4 H-bridge)
- [ ] Motor encoder reading
- [ ] PID motor control
- [ ] IMU (LSM6DS3TR-C) driver
- [ ] Bluetooth LE

## Contributing

Contributions welcome! See [CONTRIBUTING.md](CONTRIBUTING.md) for the full guide.

**Tip:** This is complex bare-metal embedded code — direct register access, ARM exceptions, linker scripts. We strongly recommend using an AI coding assistant (GitHub Copilot, Cursor, etc.) to help navigate STM32 registers, ARM Cortex-M internals, and the pybricks reference source. We built this entire project with AI assistance and it was essential.

```bash
git clone https://github.com/YOUR_USERNAME/LegoSpikeRust.git
cd LegoSpikeRust
# See CONTRIBUTING.md for toolchain setup and workflow
```

## License

This project is for educational and personal use. Hardware documentation was derived from studying the [Pybricks](https://github.com/pybricks/pybricks-micropython) open-source project (MIT License).
