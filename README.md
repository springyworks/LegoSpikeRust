# LegoSpikeRust

**Bare-metal Rust firmware for the LEGO SPIKE Prime / Mindstorms Robot Inventor Hub (51515)**

No OS. No HAL crate. No RTOS. Just Rust, direct register access, and a resident debug monitor — running on a Cortex-M4F at 96 MHz.

![LEGO Hub running Rust firmware](resources/lego-hub-rust-demo.gif)

## What This Is

A from-scratch Rust firmware stack for the LEGO Education SPIKE Prime Hub (STM32F413VGT6), built entirely without an operating system. The only "OS" here is a 29 KB Rust debug monitor that lives in flash and supervises application code — with an always-on USB CLI that stays responsive even while apps run.

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

1.  **LEGO DFU Bootloader** (factory) → validates and jumps to...
2.  **Rust Debug Monitor** (0x08008000) → USB CDC serial shell, or jumps to...
3.  **Application** (0x08010000) → user firmware with trampoline hooks back to monitor

## Features

### Debug Monitor (`monitor/` — 2,066 lines of Rust, 29 KB binary)

A resident embedded debug monitor providing:

*   **Always-on USB CLI** — SysTick at 1 kHz polls USB in the ISR; shell stays responsive while apps run. Type `stop`, `status`, or `peek` at any time.
*   **USB CDC serial shell** — interactive command line over USB
*   **Memory inspection** — `peek`, `poke`, `dump` (hex dump with ASCII)
*   **Hardware breakpoints** — FPB (Flash Patch and Breakpoint unit), up to 6 breakpoints
*   **Data watchpoints** — DWT (Data Watchpoint and Trace unit), up to 4 watchpoints
*   **Single-step execution** — ARM DebugMonitor exception (MON\_STEP)
*   **Register inspection** — `regs`, `set r0 value` while halted at breakpoint
*   **ARM MPU protection** — monitor flash (32 KB) is read-only, monitor RAM (8 KB) is privileged-only. A rogue app cannot erase or corrupt the monitor.
*   **Flash write protection (WRP)** — STM32 option-byte write-protect on sectors 0–3 (bootloader + monitor). Hardware-enforced; even direct FLASH controller writes are blocked.
*   **Center button** — short press = stop app / toggle demo / **pause motors**; long press (3 s) = power off (PA13 release, like Pybricks)
*   **Motor freeze (pause/play)** — press center button while app runs to freeze all motor outputs (safety stop); press again to resume. Also available via `pause`/`resume` CLI commands.
*   **Serial app upload** — flash new application code without DFU mode (`upload` command + `upload.py`)
*   **Motor diagnostics** — `motors` command dumps GPIO/TIM1 config for all 6 ports
*   **DFU entry** — `dfu` command to enter the STM32 system bootloader. [How to do DFU](https://github.com/orgs/pybricks/discussions/688), it can happen that one has to temporarily remove the battery and the USB to reset the hub completely first before going into DFU

### Application Trampolines

Apps link against the monitor via RAM-based function pointers:

*   `0x2004FFE0` — DebugMonitor handler (breakpoints, single-step)
*   `0x2004FFE4` — SysTick handler (center button polling via ADC)

Apps are simple `#[no_std]` binaries with inline assembly trampolines that forward exceptions to the monitor.

### Workspace Crates

| Crate | Purpose | Binary Size |
| --- | --- | --- |
| `monitor/` | Resident debug monitor + USB serial shell | 29 KB |
| `motor-test/` | Minimal motor demo app (random A+B) | 2.3 KB |
| `led-test/` | LED matrix test app (TLC5955 driver) | 3.3 KB |
| `hub-motors/` | Motor control with RTIC v2 + defmt | — |
| `bootloader/` | Simple bootloader (superseded by monitor) | — |
| `spike-rt-sim/` | Native Linux simulator (Pybricks pbio + virtual motors) | — |

## Hardware

*   **MCU**: STM32F413VGT6 — ARM Cortex-M4F, 96 MHz, 1 MB Flash, 320 KB RAM
*   **LED Matrix**: TLC5955 48-channel PWM driver on SPI1 (5×5 RGB matrix + status LEDs)
*   **Motors**: 6 ports (A–F), H-bridge via TIM1/TIM4 PWM, quadrature encoders
*   **IMU**: LSM6DS3TR-C (accelerometer + gyroscope)
*   **Center Button**: Resistor ladder on PC4 (ADC1 channel 14) — not a simple GPIO!
*   **USB**: OTG FS (PA11/PA12), CDC serial for monitor communication
*   **Power**: PA13 = power hold (must set HIGH immediately on boot)

## Acknowledgments — Pybricks

This project heavily references and reuses knowledge from the [**Pybricks**](https://github.com/pybricks/pybricks-micropython) project. The Pybricks C source code (`lib/pbio/`) was invaluable for reverse-engineering the SPIKE Prime hardware:

*   **Resistor ladder button driver** (`drv/button/button_resistor_ladder.c`) — discovered that the center button uses an ADC-based resistor ladder, not simple GPIO. Our SysTick handler does a single-shot ADC conversion on channel 14 to detect button presses.
*   **TLC5955 LED driver** (`drv/pwm/pwm_tlc5955_stm32.c`) — learned the control register must be sent twice, the channel-to-LED mapping, and the SPI/LAT/GSCLK pin configuration.
*   **Motor port GPIO mapping** — port pin assignments (ENA/ENB/IN1/IN2), encoder timer channels, H-bridge topology.
*   **Platform configuration** (`lib/pbio/platform/prime_hub/`) — clock tree (PLL settings for 96 MHz + 48 MHz USB), GPIO alternate functions, peripheral addresses.
*   **USB OTG configuration** — FIFO depths, endpoint counts, VBUS detection via PA9.

The Pybricks source tree is included as a reference in `pybricks-micropython/`. No Pybricks C code is compiled or linked — all firmware is written in Rust, but the hardware knowledge came from studying their excellent codebase.

See also: [Pybricks discussion on custom firmware for SPIKE Prime](https://github.com/orgs/pybricks/discussions/688)

## Build & Deploy

### Prerequisites

```
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

```
cd monitor && cargo build --release
arm-none-eabi-objcopy -O binary target/thumbv7em-none-eabihf/release/monitor /tmp/monitor.bin
# Put hub in DFU mode (hold center button while plugging USB)
dfu-util -d 0694:0011 -a 0 -s 0x08008000:leave -D /tmp/monitor.bin
```

Or use the dev script:

```
./dev.sh --monitor
```

### Upload Applications (no DFU needed!)

Once the monitor is flashed, apps are uploaded over serial:

```
cd led-test && cargo build --release
arm-none-eabi-objcopy -O binary target/thumbv7em-none-eabihf/release/led-test led-test.bin
python3 upload.py led-test.bin /dev/ttyACM0
```

### Monitor Commands

```
> help              — show all commands
> upload            — flash app binary over serial
> run               — launch app (monitor stays alive)
> motors            — dump motor GPIO/TIM1 config
> peek <addr>       — read 32-bit word (works while app runs)
> peek16/peek8      — read 16/8-bit
> poke <addr> <val> — write 32-bit word
> dump <addr> <len> — hex dump (max 256 bytes)
> bp <addr>         — set hardware breakpoint
> watch <addr>      — set data watchpoint
> dbgmon            — enable DebugMonitor exception
> protect           — enable flash WRP (sectors 0-3)
> unprotect         — disable flash WRP (for DFU updates)
> off / poweroff    — power off hub (release PA13)
> dfu               — enter STM32 system DFU
> reboot            — reset MCU

While app runs (always-on):
> stop / kill       — stop app, return to monitor
> status            — show app state
> peek <addr>       — read memory live
> Ctrl+C            — emergency stop

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

1.  Configures the PLL (96 MHz HCLK, 48 MHz USB)
2.  Sets up ARM MPU (monitor flash = read-only, monitor RAM = privileged-only)
3.  Optionally enables STM32 flash write protection (option bytes WRP, sectors 0–3)
4.  Initializes USB OTG FS as CDC serial
5.  Runs a polling loop reading serial commands
6.  Uses SysTick at **1 kHz** to poll USB in the ISR — the shell stays alive even while apps run in thread mode
7.  Uses ARM DebugMonitor exception (not halt-mode debug) for breakpoints and single-step
8.  Long-press center button (3 s) triggers power off (PA13 release, motors stop)

### Center Button — The Resistor Ladder Trap

The SPIKE Prime center button is **not** a simple GPIO digital input. It's part of a resistor ladder (multiple buttons sharing one analog ADC pin, PC4). When pressed, the voltage drops from ~3.3V to ~2.5V — still above the digital GPIO threshold of ~1.65V. You **must** use ADC to detect it. This took considerable reverse-engineering of the Pybricks source to discover.

### TLC5955 — The Double-Latch Gotcha

The TLC5955 LED driver requires the control register to be sent **twice** before grayscale data produces visible output. This is per the TLC5955 datasheet but easy to miss. The control register sets dot correction, max current, brightness, and operating modes.

## Project Status

*   Custom Rust bootloader with DFU support
*   Resident debug monitor with USB serial shell
*   **Always-on CLI** — shell stays responsive while apps run (SysTick 1 kHz USB polling)
*   Memory inspection (peek/poke/dump)
*   Hardware breakpoints (FPB) and data watchpoints (DWT)
*   Single-step debugging via DebugMonitor exception
*   Center button stop (ADC resistor ladder) + long-press power off
*   Serial app upload (no DFU for app dev)
*   ARM MPU protection (monitor flash RO, monitor RAM priv-only)
*   STM32 flash write protection (option bytes WRP, sectors 0–3)
*   TLC5955 LED matrix driver (5×5 + status LEDs)
*   Motor PWM control (TIM1/TIM4 H-bridge)
*   Motor encoder reading
*   PID motor control
*   IMU (LSM6DS3TR-C) driver
*   Bluetooth LE

## Contributing

Contributions welcome! See [CONTRIBUTING.md](CONTRIBUTING.md) for the full guide.

**Tip:** This is complex bare-metal embedded code — direct register access, ARM exceptions, linker scripts. We strongly recommend using an AI coding assistant (GitHub Copilot, Cursor, etc.) to help navigate STM32 registers, ARM Cortex-M internals, and the pybricks reference source. We built this entire project with AI assistance and it was essential.

```
git clone https://github.com/YOUR_USERNAME/LegoSpikeRust.git
cd LegoSpikeRust
# See CONTRIBUTING.md for toolchain setup and workflow
```

## License

This project is for educational and personal use. Hardware documentation was derived from studying the [Pybricks](https://github.com/pybricks/pybricks-micropython) open-source project (MIT License).