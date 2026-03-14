# Pushing SPIKE Prime Limits: Rust & Assembly

It is exciting to see you pushing the SPIKE Prime hub beyond its default limits. Since you are looking for memory safety (Rust) and raw execution speed (Assembly), you are moving into "Bare Metal" territory.

The SPIKE Prime Hub uses an **STM32F413 (ARM Cortex-M4F)** microcontroller. This is a very capable chip, and you are right that the default LEGO OS (MicroPython) adds significant overhead.

## 1. The Direct Successor to EV3 OSEK: SPIKE-RT

If you loved the OSEK/EV3RT environment, you should look at **SPIKE-RT**.

*   **What it is:** A real-time software platform for SPIKE Prime based on the **TOPPERS/ASP3 kernel** (a 3rd-generation ITRON-compliant RTOS, which is the Japanese standard for industrial and automotive real-time systems, similar in philosophy to OSEK).
*   **Why it fits you:** It allows for programming in C and Assembly directly. It bypasses the MicroPython VM entirely, providing deterministic, high-speed execution.
*   **Language:** While natively C, it is the perfect "host" for Rust (see below).
*   **Repository:** You can find it on GitHub under `spike-rt/spike-rt`.

## 2. Rust on the SPIKE Hub

Because the hub is a standard STM32F4 series chip, you can use the `stm32f4xx-hal` crate in Rust.

*   **Memory Safety:** Rust’s ownership model will provide the "mem-safety" you requested, preventing null pointers or buffer overflows—critical when you are manually controlling high-torque motors.
*   **Assembly Integration:** You can easily "churn out" assembly by using the `asm!` macro in Rust or by linking `.s` files. The Cortex-M4F core supports the Thumb-2 instruction set, allowing for highly optimized DSP-like calculations.
*   **TECS/Rust:** There is active research (check the TECS/Rust project) specifically aimed at using Rust for component-based development on TOPPERS/ASP3 (the same RTOS used in SPIKE-RT).

## 3. Comparison of OS Options

| Feature | LEGO OS 3 | Pybricks | SPIKE-RT (TOPPERS) | Bare Metal (Rust/C) |
| :--- | :--- | :--- | :--- | :--- |
| **Runtime** | MicroPython | MicroPython | Real-Time C | Rust (no OS) |
| **Speed** | Slow (Interpreted) | Optimized (Interpreted) | Very Fast (Native) | Maximum (Native) |
| **Safety** | Managed | Managed | Manual (C) | Compiler-Enforced |
| **Standard** | None | None | ITRON/ASP3 | Custom |

## 4. How to Flash "Bare Metal"

To run Rust or SPIKE-RT, you must put the hub into **DFU (Device Firmware Update) Mode**:

1.  Disconnect the Hub from USB.
2.  Hold down the **Bluetooth button**.
3.  Connect the USB cable while holding the button.
4.  The button will pulse **purple/pink**.
5.  On Ubuntu 24, you can then use `dfu-util` to flash your compiled Rust binary (`.bin`) or your SPIKE-RT image.

### Recommended Next Steps

*   Check out SPIKE-RT on GitHub to see how they handle the pin mappings for the 6 ports and the 5x5 LED matrix.
*   Use the `stm32f4xx-hal` crate with the feature `stm32f413` to start a Rust project.
*   Explore the "embedded-hal" ecosystem to see if you can find drivers for the LPF2 (LEGO Powered Up) protocol used by the motors.

> **Note:** Would you like me to find a basic Rust `Cargo.toml` and hardware initialization snippet specifically for the STM32F413 chip in your hub?

---

## FAQ: Safety & Recovery

The good news is that the LEGO SPIKE Prime hub is remarkably resilient.

### 1. Can I revert to HubOS 3?

**Yes, absolutely.** You can revert to the official LEGO firmware at any time. There are two main ways to do this:

*   **Official LEGO Way:** If you connect your hub in DFU Mode (Update Mode) and open the SPIKE Web App, the app will typically detect that the hub needs an update and offer to "Update" it. This process effectively overwrites whatever is on the hub (like spike-rt) with the official HubOS 3.
*   **Pybricks Tool:** The Pybricks Restore Tool is often the most reliable way to get back to factory settings. It has a specific "Restore Official LEGO Firmware" button that works directly in the browser.

### 2. Is there a danger of "bricking" the hub?

The risk of a "hard brick" (permanent death) is **extremely low**.

The SPIKE Prime uses an STM32F413 microcontroller. The "Update Mode" (DFU) is actually hard-coded into the chip's **ROM (Read-Only Memory)** by the manufacturer (STMicroelectronics). Because it is in ROM, your code or a custom OS cannot delete or break the bootloader.

> **Note:** If a flash fails, the hub might seem "dead" (no lights, no response), but as long as you can trigger the Bluetooth/USB sequence, you can always re-flash it.

### 3. The "Special" USB + Bluetooth Sequence

This sequence is how you force the hardware into DFU (Device Firmware Update) Mode, bypassing whatever software is currently running.

**How to enter DFU Mode correctly:**

1.  Unplug the USB cable and ensure the hub is completely off.
2.  Press and hold the **Bluetooth button** (the top-right button with the light).
3.  While still holding the button, plug in the USB cable to your computer.
4.  Continue holding the button until the light starts pulsing **purple/pink** (or a cycling pattern).
5.  On Ubuntu 24, you can verify the hub is ready by typing `lsusb` in the terminal; it should show up as an "STMicroelectronics STM Device in DFU Mode".

### spike-rt vs. HubOS 3

| Feature | HubOS 3 (Official) | spike-rt (Bare Metal) |
| :--- | :--- | :--- |
| **Foundation** | MicroPython VM | TOPPERS/ASP3 (Real-Time OS) |
| **Language** | Python | C / Rust / Assembly |
| **Predictability** | Soft Real-Time (Garbage Collection) | Hard Real-Time (Deterministic) |
| **Control** | High-level API | Direct Register Access |

If you are aiming for Rust, **spike-rt** is the better foundation because it handles the low-level hardware initialization (clocks, power management) while letting you run native code without the memory overhead of a Python interpreter.
