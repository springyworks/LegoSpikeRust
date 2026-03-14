//! Embedded Debug Monitor for LEGO SPIKE Prime Hub (STM32F413VGT6)
//!
//! An evolution of the bootloader — provides USB serial shell, memory
//! inspection, and ARM DebugMonitor-based breakpoint/step support.
//!
//! Flash layout:
//!   0x08000000  LEGO DFU bootloader     (32 KB, factory, untouched)
//!   0x08008000  This monitor            (32 KB)
//!   0x08010000  Application firmware    (960 KB)
//!
//! Boot sequence:
//!   1. PA13 HIGH (power hold) — before anything else
//!   2. Check DFU magic in RAM — if set, enter STM32 system DFU
//!   3. Check center button (held = enter DFU)
//!   4. Init PLL (96 MHz SYSCLK, 48 MHz USB clock)
//!   5. Init USB CDC serial
//!   6. If USB host connected: interactive monitor shell
//!   7. If no connection within 3s: validate app → jump to it
//!   8. If no valid app and no USB: enter STM32 system DFU

#![no_std]
#![no_main]
#![allow(static_mut_refs)]

use core::ptr;

use synopsys_usb_otg::UsbBus;
use usb_device::prelude::*;
use usbd_serial::SerialPort;

// ════════════════════════════════════════════════════════════════
// Panic handler — minimal, no allocations
// ════════════════════════════════════════════════════════════════

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    // In a monitor, just hang — the LEGO DFU button-hold is recovery
    loop {
        cortex_m::asm::wfi();
    }
}

// ════════════════════════════════════════════════════════════════
// Constants
// ════════════════════════════════════════════════════════════════

const APP_ADDR: u32 = 0x0801_0000;
const DFU_MAGIC_ADDR: *mut u32 = 0x2004_FFF0 as *mut u32;
const DFU_MAGIC_VALUE: u32 = 0xDEAD_B007;

// Register bases
const RCC: u32 = 0x4002_3800;
const FLASH_R: u32 = 0x4002_3C00;
const GPIOA: u32 = 0x4002_0000;
const GPIOB: u32 = 0x4002_0400;
const GPIOC: u32 = 0x4002_0800;
const ADC1: u32 = 0x4001_2000;
const SYSCFG: u32 = 0x4001_3800;
const SPI1: u32 = 0x4001_3000;
const TIM12: u32 = 0x4000_1800;

// Center button: PC4, ADC1 channel 14
const BUTTON_THRESHOLD: u32 = 2879;

// Debug registers
const SCB_DHCSR: *const u32 = 0xE000_EDF0 as *const u32;
const SCB_DEMCR: *mut u32 = 0xE000_EDFC as *mut u32;
#[allow(dead_code)]
const SCB_DFSR: *mut u32 = 0xE000_ED30 as *mut u32;
const SCB_SHPR3: *mut u32 = 0xE000_ED20 as *mut u32;

// FPB (Flash Patch and Breakpoint)
const FPB_CTRL: *mut u32 = 0xE000_2000 as *mut u32;
const FPB_COMP_BASE: u32 = 0xE000_2008;
const FPB_MAX_COMP: usize = 6; // STM32F413 has 6 FP comparators

// DWT (Data Watchpoint and Trace)
const DWT_COMP_BASE: u32 = 0xE000_1020;
const DWT_FUNCTION_OFFSET: u32 = 0x08;
const DWT_COMP_SPACING: u32 = 0x10;
const DWT_MAX_COMP: usize = 4; // STM32F413 has 4 DWT comparators

// Fixed RAM locations for monitor↔app trampolines
const TRAMPOLINE_ADDR: *mut u32 = 0x2004_FFE0 as *mut u32;
const SYSTICK_TRAMPOLINE: *mut u32 = 0x2004_FFE4 as *mut u32;

// SysTick registers
const SYST_CSR: *mut u32 = 0xE000_E010 as *mut u32;
const SYST_RVR: *mut u32 = 0xE000_E014 as *mut u32;
const SYST_CVR: *mut u32 = 0xE000_E018 as *mut u32;

// ════════════════════════════════════════════════════════════════
// USB OTG FS peripheral implementation
// ════════════════════════════════════════════════════════════════

pub struct UsbFs {
    _private: (),
}

unsafe impl Sync for UsbFs {}
unsafe impl Send for UsbFs {}

unsafe impl synopsys_usb_otg::UsbPeripheral for UsbFs {
    const REGISTERS: *const () = 0x5000_0000 as *const ();
    const HIGH_SPEED: bool = false;
    const FIFO_DEPTH_WORDS: usize = 320;
    const ENDPOINT_COUNT: usize = 6;

    fn enable() {
        unsafe {
            // Enable USB OTG FS clock (RCC_AHB2ENR bit 7)
            let ahb2enr = (RCC + 0x34) as *mut u32;
            ptr::write_volatile(ahb2enr, ptr::read_volatile(ahb2enr) | (1 << 7));

            // Reset USB OTG FS (RCC_AHB2RSTR bit 7)
            let ahb2rstr = (RCC + 0x14) as *mut u32;
            ptr::write_volatile(ahb2rstr, ptr::read_volatile(ahb2rstr) | (1 << 7));
            ptr::write_volatile(ahb2rstr, ptr::read_volatile(ahb2rstr) & !(1 << 7));
        }
    }

    fn ahb_frequency_hz(&self) -> u32 {
        96_000_000
    }
}

// ════════════════════════════════════════════════════════════════
// Low-level register helpers
// ════════════════════════════════════════════════════════════════

#[inline(always)]
unsafe fn reg_write(base: u32, offset: u32, val: u32) {
    ptr::write_volatile((base + offset) as *mut u32, val);
}

#[inline(always)]
unsafe fn reg_read(base: u32, offset: u32) -> u32 {
    ptr::read_volatile((base + offset) as *const u32)
}

#[inline(always)]
unsafe fn reg_modify(base: u32, offset: u32, clear: u32, set: u32) {
    let v = reg_read(base, offset);
    reg_write(base, offset, (v & !clear) | set);
}

#[inline(always)]
fn busy_wait(cycles: u32) {
    cortex_m::asm::delay(cycles);
}

// ════════════════════════════════════════════════════════════════
// Clock configuration: HSE 16 MHz → PLL → 96 MHz SYSCLK + 48 MHz USB
// ════════════════════════════════════════════════════════════════

unsafe fn init_clocks() {
    // 3 wait states for 96 MHz
    reg_modify(FLASH_R, 0x00, 0xF, 3);

    // Enable HSE
    reg_modify(RCC, 0x00, 0, 1 << 16);
    while reg_read(RCC, 0x00) & (1 << 17) == 0 {} // wait HSERDY

    // PLL: HSE / 16 * 192 / 2 = 96 MHz, PLLQ = 192/4 = 48 MHz for USB
    //  PLLM=16, PLLN=192, PLLP=0(/2), PLLSRC=HSE, PLLQ=4
    reg_write(RCC, 0x04, 16 | (192 << 6) | (0 << 16) | (1 << 22) | (4 << 24));

    // Enable PLL
    reg_modify(RCC, 0x00, 0, 1 << 24);
    while reg_read(RCC, 0x00) & (1 << 25) == 0 {} // wait PLLRDY

    // APB1 prescaler = /4 (24 MHz), APB2 = /1 (96 MHz)
    // PPRE1 = 5 (bits 12:10 = 101), PPRE2 = 0 (bits 15:13 = 000)
    reg_modify(RCC, 0x08, 0xFCF0, 0x1000);

    // Switch SYSCLK to PLL (SW = 10)
    reg_modify(RCC, 0x08, 0x3, 0x2);
    while (reg_read(RCC, 0x08) & 0xC) != 0x8 {} // wait SWS = PLL
}

// ════════════════════════════════════════════════════════════════
// USB GPIO init: PA11 (D-) / PA12 (D+) as AF10, PA9 VBUS input
// ════════════════════════════════════════════════════════════════

// ════════════════════════════════════════════════════════════════
// Startup beep: PA4 (DAC out) + PC10 (amp enable)
// ════════════════════════════════════════════════════════════════

unsafe fn beep(duration_ms: u32, freq_hz: u32) {
    // Enable GPIOA + GPIOC clocks
    reg_modify(RCC, 0x30, 0, (1 << 0) | (1 << 2));
    let _ = reg_read(RCC, 0x30);

    // PC10 = output HIGH (speaker amplifier enable)
    reg_modify(GPIOC, 0x00, 3 << 20, 1 << 20);
    reg_write(GPIOC, 0x18, 1 << 10);

    // PA4 = output
    reg_modify(GPIOA, 0x00, 3 << 8, 1 << 8);

    // Toggle PA4 as crude square wave
    let half_period_cycles = 96_000_000 / (freq_hz * 2);
    let toggles = duration_ms * freq_hz * 2 / 1000;
    for _ in 0..toggles {
        reg_write(GPIOA, 0x18, 1 << 4);        // set PA4
        busy_wait(half_period_cycles);
        reg_write(GPIOA, 0x18, 1 << (4 + 16)); // clear PA4
        busy_wait(half_period_cycles);
    }

    // Disable amplifier, restore PA4 to analog
    reg_write(GPIOC, 0x18, 1 << (10 + 16));
    reg_modify(GPIOA, 0x00, 3 << 8, 3 << 8);
}

// ════════════════════════════════════════════════════════════════
// Status LED via TLC5955: light center button ring GREEN
// SPI1: MOSI=PA7(AF5), SCK=PA5(AF5), LAT=PA15(GPIO)
// GSCLK: TIM12_CH2 on PB15(AF9) — ~9.6 MHz PWM clock
// ════════════════════════════════════════════════════════════════

unsafe fn init_status_led() {
    // Enable clocks: GPIOA(0), GPIOB(1), SPI1(12 on APB2), TIM12(6 on APB1)
    reg_modify(RCC, 0x30, 0, (1 << 0) | (1 << 1)); // AHB1ENR: GPIOA, GPIOB
    reg_modify(RCC, 0x44, 0, 1 << 12);              // APB2ENR: SPI1
    reg_modify(RCC, 0x40, 0, 1 << 6);               // APB1ENR: TIM12
    let _ = reg_read(RCC, 0x30);

    // PA5 = AF5 (SPI1_SCK), PA7 = AF5 (SPI1_MOSI)
    reg_modify(GPIOA, 0x20, (0xF << 20) | (0xF << 28), (5 << 20) | (5 << 28)); // AFRL
    reg_modify(GPIOA, 0x00, (3 << 10) | (3 << 14), (2 << 10) | (2 << 14));     // MODER = AF
    reg_modify(GPIOA, 0x08, (3 << 10) | (3 << 14), (3 << 10) | (3 << 14));     // OSPEEDR = very high

    // PA15 = GPIO output (LAT) — clear JTDI AF first
    reg_modify(GPIOA, 0x24, 0xF << 28, 0); // AFRH: PA15 AF = 0
    reg_modify(GPIOA, 0x00, 3 << 30, 1 << 30);
    reg_write(GPIOA, 0x18, 1 << (15 + 16)); // LAT LOW initially

    // PB15 = AF9 (TIM12_CH2 = GSCLK)
    reg_modify(GPIOB, 0x24, 0xF << 28, 9 << 28); // AFRH
    reg_modify(GPIOB, 0x00, 3 << 30, 2 << 30);   // MODER = AF
    reg_modify(GPIOB, 0x08, 3 << 30, 3 << 30);   // OSPEEDR = very high

    // ── TIM12 CH2: 9.6 MHz PWM (GSCLK for TLC5955) ─────
    reg_write(TIM12, 0x28, 0);    // PSC = 0
    reg_write(TIM12, 0x2C, 4);    // ARR = 4 (48MHz/5 = 9.6 MHz)
    reg_write(TIM12, 0x18, (6 << 12) | (1 << 11)); // OC2M=PWM1, OC2PE
    reg_write(TIM12, 0x38, 2);    // CCR2 = 2 (50% duty)
    reg_write(TIM12, 0x20, 1 << 4); // CCER: CC2E = 1
    reg_write(TIM12, 0x00, 1);      // CR1: CEN = 1

    // ── SPI1: master, 8-bit, software NSS, ~6 MHz ───────
    reg_write(SPI1, 0x00, (1 << 2) | (3 << 3) | (1 << 9) | (1 << 8)); // MSTR|BR=011|SSM|SSI
    reg_modify(SPI1, 0x00, 0, 1 << 6); // SPE: enable SPI

    // Helper: send 97 bytes over SPI1 and pulse LAT
    let spi_send_latch = |data: &[u8; 97]| {
        for &byte in data {
            while reg_read(SPI1, 0x08) & (1 << 1) == 0 {}
            reg_write(SPI1, 0x0C, byte as u32);
        }
        while reg_read(SPI1, 0x08) & (1 << 7) != 0 {}
        reg_write(GPIOA, 0x18, 1 << 15);
        busy_wait(100);
        reg_write(GPIOA, 0x18, 1 << (15 + 16));
    };

    // ── CONTROL REGISTER (must send TWICE per TLC5955 spec) ──
    // dc=127, mc=0(3.2mA), bc=127, dsprpt=1, espwm=1, lsdvlt=1
    let mut ctrl = [0u8; 97];
    ctrl[0] = 1;      // control mode
    ctrl[1] = 0x96;   // constant
    ctrl[50] = (1 << 2) | (1 << 1);     // LSDVLT=1, ESPWM=1
    ctrl[51] = (1 << 6) | (127 >> 1);   // DSPRPT=1, BC[6:1]
    ctrl[52] = ((127u16 << 7) as u8) | 127; // BC[0]+BC_G
    ctrl[53] = (127 << 1) as u8;        // BC_R, MC[2]=0
    ctrl[54] = 0;                        // MC=0
    for b in ctrl[55..97].iter_mut() { *b = 0xFF; } // DC=127 all channels

    spi_send_latch(&ctrl);
    spi_send_latch(&ctrl); // twice!

    // ── GREYSCALE DATA: channel 4 = green, modest brightness ──
    // Channel N → frame[N*2+1] (hi), frame[N*2+2] (lo)
    let mut frame = [0u8; 97];
    frame[0] = 0; // GS mode
    frame[4 * 2 + 1] = 0x20; // channel 4 (green) = 0x2000
    frame[4 * 2 + 2] = 0x00;

    spi_send_latch(&frame);
}

unsafe fn init_usb_pins() {
    // Enable GPIOA clock (RCC_AHB1ENR bit 0) — may already be on from pre_init
    reg_modify(RCC, 0x30, 0, 1 << 0);
    let _ = reg_read(RCC, 0x30);

    // PA11 = AF10 (USB_DM), PA12 = AF10 (USB_DP) — alternate function mode
    // AFRH bits: PA11 = bits 15:12 = 10 (0xA), PA12 = bits 19:16 = 10 (0xA)
    reg_modify(GPIOA, 0x24, (0xF << 12) | (0xF << 16), (10 << 12) | (10 << 16));
    // MODER: PA11 bits 23:22 = 10 (AF), PA12 bits 25:24 = 10 (AF)
    reg_modify(GPIOA, 0x00, (3 << 22) | (3 << 24), (2 << 22) | (2 << 24));
    // High speed for USB pins
    reg_modify(GPIOA, 0x08, (3 << 22) | (3 << 24), (3 << 22) | (3 << 24));

    // PA9 = VBUS detect (input, no pull — already reset default)
    reg_modify(GPIOA, 0x00, 3 << 18, 0);
}

// ════════════════════════════════════════════════════════════════
// Center button (same as bootloader)
// ════════════════════════════════════════════════════════════════

unsafe fn read_center_button() -> bool {
    // Enable GPIOC + ADC1 clocks
    reg_modify(RCC, 0x30, 0, 1 << 2);
    reg_modify(RCC, 0x44, 0, 1 << 8);
    let _ = reg_read(RCC, 0x44);

    // ADC: single conversion, channel 14, 480-cycle sample time
    reg_write(ADC1, 0x2C, 0);
    reg_write(ADC1, 0x34, 14);
    reg_write(ADC1, 0x0C, 7 << 12);

    // Power on, stabilize
    reg_write(ADC1, 0x08, 1);
    busy_wait(160_000);

    // Start + wait
    let cr2 = reg_read(ADC1, 0x08);
    reg_write(ADC1, 0x08, cr2 | (1 << 30));
    while reg_read(ADC1, 0x00) & (1 << 1) == 0 {}
    let val = reg_read(ADC1, 0x4C);
    reg_write(ADC1, 0x08, 0); // power off

    val <= BUTTON_THRESHOLD
}

// ════════════════════════════════════════════════════════════════
// PA13 power hold — runs BEFORE .bss/.data init
// ════════════════════════════════════════════════════════════════

#[cortex_m_rt::pre_init]
unsafe fn pre_init() {
    let rcc_ahb1enr = (RCC + 0x30) as *mut u32;
    ptr::write_volatile(rcc_ahb1enr, ptr::read_volatile(rcc_ahb1enr) | 1);
    let _ = ptr::read_volatile(rcc_ahb1enr);

    // PA13 → output, HIGH
    let moder = GPIOA as *mut u32;
    let v = ptr::read_volatile(moder);
    ptr::write_volatile(moder, (v & !(3 << 26)) | (1 << 26));
    ptr::write_volatile((GPIOA + 0x18) as *mut u32, 1 << 13);
}

// ════════════════════════════════════════════════════════════════
// Enter STM32 system bootloader (USB DFU at 0x1FFF0000)
// ════════════════════════════════════════════════════════════════

unsafe fn enter_system_dfu() -> ! {
    cortex_m::interrupt::disable();
    ptr::write_volatile(0xE000_E010 as *mut u32, 0); // disable SysTick
    for i in 0..8u32 {
        ptr::write_volatile((0xE000_E180 + i * 4) as *mut u32, 0xFFFF_FFFF);
        ptr::write_volatile((0xE000_E280 + i * 4) as *mut u32, 0xFFFF_FFFF);
    }
    reg_modify(RCC, 0x44, 0, 1 << 14); // enable SYSCFG clock
    let _ = reg_read(RCC, 0x44);
    reg_write(SYSCFG, 0x00, 0x01); // remap system memory
    let sp = ptr::read_volatile(0x1FFF_0000 as *const u32);
    let pc = ptr::read_volatile(0x1FFF_0004 as *const u32);
    core::arch::asm!("MSR MSP, {}", in(reg) sp);
    let entry: extern "C" fn() -> ! = core::mem::transmute(pc);
    entry();
}

// ════════════════════════════════════════════════════════════════
// Jump to application
// ════════════════════════════════════════════════════════════════

unsafe fn jump_to_app(sp: u32, pc: u32) -> ! {
    cortex_m::interrupt::disable();
    // Don't disable SysTick — monitor uses it to poll center button
    ptr::write_volatile(0xE000_ED04 as *mut u32, 1 << 25); // clear PendSV
    ptr::write_volatile(0xE000_ED08 as *mut u32, APP_ADDR); // VTOR
    core::arch::asm!("MSR MSP, {}", in(reg) sp);
    cortex_m::interrupt::enable(); // re-enable so SysTick fires in app
    let entry: extern "C" fn() -> ! = core::mem::transmute(pc);
    entry();
}

// ════════════════════════════════════════════════════════════════
// DebugMonitor support
// ════════════════════════════════════════════════════════════════

/// Enable DebugMonitor exception. Returns false if halting debug is active.
unsafe fn debug_monitor_enable() -> bool {
    // Check that halting debug is NOT active (C_DEBUGEN must be 0)
    if ptr::read_volatile(SCB_DHCSR) & 1 != 0 {
        return false;
    }

    // Enable DebugMonitor exception (MON_EN = bit 16)
    let demcr = ptr::read_volatile(SCB_DEMCR);
    ptr::write_volatile(SCB_DEMCR, demcr | (1 << 16));

    // Set DebugMonitor priority to lowest (0xFF) so all ISRs run above it
    let shpr3 = ptr::read_volatile(SCB_SHPR3);
    ptr::write_volatile(SCB_SHPR3, (shpr3 & !0xFF) | 0xFF);

    true
}

/// Set a hardware breakpoint using the FPB.
/// comp_id: 0..5 (STM32F413 has 6 comparators)
/// addr: instruction address in code region (< 0x2000_0000)
unsafe fn fpb_set_breakpoint(comp_id: usize, addr: u32) -> bool {
    if comp_id >= FPB_MAX_COMP || addr >= 0x2000_0000 {
        return false;
    }

    // Enable FPB (KEY=1, ENABLE=1)
    ptr::write_volatile(FPB_CTRL, 0x3);

    // FP_COMP: addr[28:2] in bits 28:2, addr[1:0] → REPLACE in bits 31:30, enable bit 0
    let replace = if addr & 0x2 == 0 { 1u32 } else { 2u32 };
    let fp_comp = (addr & !0x3) | 0x1 | (replace << 30);
    let comp_addr = (FPB_COMP_BASE + (comp_id as u32) * 4) as *mut u32;
    ptr::write_volatile(comp_addr, fp_comp);
    true
}

/// Clear a hardware breakpoint.
unsafe fn fpb_clear_breakpoint(comp_id: usize) {
    if comp_id < FPB_MAX_COMP {
        let comp_addr = (FPB_COMP_BASE + (comp_id as u32) * 4) as *mut u32;
        ptr::write_volatile(comp_addr, 0);
    }
}

/// Clear all hardware breakpoints.
unsafe fn fpb_clear_all() {
    for i in 0..FPB_MAX_COMP {
        fpb_clear_breakpoint(i);
    }
}

/// Enable/disable FPB unit.
#[allow(dead_code)]
unsafe fn fpb_set_enabled(enable: bool) {
    if enable {
        ptr::write_volatile(FPB_CTRL, 0x3);
    } else {
        ptr::write_volatile(FPB_CTRL, 0x2); // KEY=1, ENABLE=0
    }
}

/// Set a DWT data watchpoint.
/// comp_id: 0..3, addr: address to watch, write_only: true = write, false = read/write
unsafe fn dwt_set_watchpoint(comp_id: usize, addr: u32, write_only: bool) -> bool {
    if comp_id >= DWT_MAX_COMP {
        return false;
    }
    let base = DWT_COMP_BASE + (comp_id as u32) * DWT_COMP_SPACING;
    ptr::write_volatile(base as *mut u32, addr); // DWT_COMPn
    ptr::write_volatile((base + 0x04) as *mut u32, 0); // DWT_MASKn = exact match
    let func = if write_only { 0x5u32 } else { 0x7u32 }; // write or read/write
    ptr::write_volatile((base + DWT_FUNCTION_OFFSET) as *mut u32, func);
    true
}

/// Clear a DWT watchpoint.
unsafe fn dwt_clear_watchpoint(comp_id: usize) {
    if comp_id < DWT_MAX_COMP {
        let base = DWT_COMP_BASE + (comp_id as u32) * DWT_COMP_SPACING;
        ptr::write_volatile((base + DWT_FUNCTION_OFFSET) as *mut u32, 0);
    }
}

// ════════════════════════════════════════════════════════════════
// SysTick handler — polls center button, pends DebugMonitor
// ════════════════════════════════════════════════════════════════

static mut BUTTON_DEBOUNCE: u8 = 0;

// SysTick handler: naked wrapper → Rust handler
core::arch::global_asm!(
    ".section .text",
    ".global SysTick",
    ".type SysTick, %function",
    ".thumb_func",
    "SysTick:",
    "push {{r4-r7, lr}}",
    "bl {handler}",
    "pop {{r4-r7, pc}}",
    handler = sym systick_handler,
);

extern "C" fn systick_handler() {
    unsafe {
        // Center button uses a resistor ladder on PC4 (ADC1 channel 14).
        // Pressed voltage (~2.5V) is above GPIO threshold, so we must use ADC.
        const ADC_CCR: *mut u32 = 0x4001_2304 as *mut u32;

        // Enable clocks: GPIOC (AHB1ENR bit 2), ADC1 (APB2ENR bit 8)
        reg_modify(RCC, 0x30, 0, 1 << 2);
        reg_modify(RCC, 0x44, 0, 1 << 8);

        // PC4 as analog (MODER bits 9:8 = 11)
        reg_modify(GPIOC, 0x00, 0x3 << 8, 0x3 << 8);

        // ADC prescaler: PCLK2/4 (24 MHz, within 36 MHz limit)
        let ccr = ptr::read_volatile(ADC_CCR);
        ptr::write_volatile(ADC_CCR, (ccr & !(3 << 16)) | (1 << 16));

        // Turn off ADC for clean config
        ptr::write_volatile((ADC1 + 0x08) as *mut u32, 0); // CR2 ADON=0
        // CR1: 12-bit, no scan
        ptr::write_volatile((ADC1 + 0x04) as *mut u32, 0);
        // Sample time ch14: 56 cycles (SMPR1 bits [14:12] = 011)
        let smpr = ptr::read_volatile((ADC1 + 0x0C) as *mut u32);
        ptr::write_volatile((ADC1 + 0x0C) as *mut u32, (smpr & !(7 << 12)) | (3 << 12));
        // 1 conversion of channel 14
        ptr::write_volatile((ADC1 + 0x2C) as *mut u32, 0);  // SQR1: L=0
        ptr::write_volatile((ADC1 + 0x34) as *mut u32, 14); // SQR3: ch14

        // Enable ADC
        ptr::write_volatile((ADC1 + 0x08) as *mut u32, 1);
        // Stabilization delay (~3µs at 96 MHz ≈ 288 cycles)
        for _ in 0..300u32 { cortex_m::asm::nop(); }

        // Start conversion
        ptr::write_volatile((ADC1 + 0x00) as *mut u32, 0);          // clear SR
        ptr::write_volatile((ADC1 + 0x08) as *mut u32, 1 | (1 << 30)); // SWSTART

        // Wait for EOC (SR bit 1)
        for _ in 0..1000u32 {
            if ptr::read_volatile((ADC1 + 0x00) as *mut u32) & (1 << 1) != 0 {
                break;
            }
        }

        let raw = (ptr::read_volatile((ADC1 + 0x4C) as *mut u32) & 0xFFF) as u16;

        // Turn off ADC
        ptr::write_volatile((ADC1 + 0x08) as *mut u32, 0);

        // Resistor ladder DEV_0 thresholds (pybricks):
        //   No press:      > 3642 (~3.3V, pulled high)
        //   Center (CH_1): 2879..3142 (~2.5V, 10k+33k divider)
        let center = raw > 2600 && raw < 3400;

        if center {
            BUTTON_DEBOUNCE = BUTTON_DEBOUNCE.saturating_add(1);
            if BUTTON_DEBOUNCE >= 50 {
                let demcr = ptr::read_volatile(SCB_DEMCR);
                ptr::write_volatile(SCB_DEMCR, demcr | (1 << 17)); // MON_PEND
                BUTTON_DEBOUNCE = 0;
            }
        } else {
            BUTTON_DEBOUNCE = 0;
        }
    }
}

/// Set up SysTick at ~100 Hz and write handler address to trampoline
unsafe fn arm_systick() {
    // 96 MHz / 100 Hz = 960000 ticks
    ptr::write_volatile(SYST_CSR, 0);          // disable
    ptr::write_volatile(SYST_RVR, 960_000 - 1); // reload
    ptr::write_volatile(SYST_CVR, 0);          // clear current
    ptr::write_volatile(SYST_CSR, 0x7);        // enable + tickint + clksource=processor

    // Write SysTick handler address to trampoline
    extern "C" { fn SysTick(); }
    ptr::write_volatile(SYSTICK_TRAMPOLINE, SysTick as *const () as u32);
}

// ════════════════════════════════════════════════════════════════
// DebugMonitor exception handler (naked ASM + Rust handler)
// ════════════════════════════════════════════════════════════════

// Global state for debug handler
static mut USB_SERIAL: Option<SerialPort<'static, UsbBus<UsbFs>>> = None;
static mut USB_DEVICE: Option<UsbDevice<'static, UsbBus<UsbFs>>> = None;
static mut USB_BUS_G: Option<usb_device::bus::UsbBusAllocator<UsbBus<UsbFs>>> = None;
static mut BP_ADDRS: [u32; FPB_MAX_COMP] = [0; FPB_MAX_COMP];
static mut WATCH_ADDRS: [u32; DWT_MAX_COMP] = [0; DWT_MAX_COMP];

// Naked DebugMonitor handler: saves context, calls Rust handler, restores and returns
core::arch::global_asm!(
    ".section .text",
    ".global DebugMonitor",
    ".type DebugMonitor, %function",
    ".thumb_func",
    "DebugMonitor:",
    // Determine which stack has the exception frame
    "tst lr, #4",
    "ite eq",
    "mrseq r0, msp",
    "mrsne r0, psp",
    // r0 = pointer to exception frame [r0,r1,r2,r3,r12,lr,pc,xpsr]
    // Save callee-saved registers + EXC_RETURN
    "push {{r4-r11, lr}}",
    "mov r1, lr",    // pass EXC_RETURN as 2nd arg
    "bl {handler}",
    // Restore callee-saved registers + EXC_RETURN
    "pop {{r4-r11, lr}}",
    "bx lr",
    handler = sym debug_monitor_handler,
);

/// Rust handler called from DebugMonitor exception.
/// exception_frame: pointer to hardware-stacked [r0,r1,r2,r3,r12,lr,pc,xpsr]
extern "C" fn debug_monitor_handler(exception_frame: *mut u32, _exc_return: u32) {
    unsafe {
        let serial = match USB_SERIAL.as_mut() { Some(s) => s, None => return };
        let usb_dev = match USB_DEVICE.as_mut() { Some(d) => d, None => return };

        // Read and clear DFSR
        let dfsr = ptr::read_volatile(SCB_DFSR);
        ptr::write_volatile(SCB_DFSR, dfsr); // W1C

        // Read exception frame
        let pc = ptr::read_volatile(exception_frame.add(6));

        // Print header
        serial_write_str(serial, "\r\n*** BREAKPOINT ***\r\n", usb_dev);
        serial_write_str(serial, "PC=0x", usb_dev);
        serial_write_hex32(serial, pc, usb_dev);
        if dfsr & 2 != 0 { serial_write_str(serial, " [BKPT]", usb_dev); }
        if dfsr & 4 != 0 { serial_write_str(serial, " [DWTTRAP]", usb_dev); }
        if dfsr & 1 != 0 { serial_write_str(serial, " [HALTED]", usb_dev); }
        serial_write_str(serial, "\r\n", usb_dev);

        // Register dump
        dbg_print_regs(serial, usb_dev, exception_frame);

        // Show instruction at PC
        let instr = ptr::read_volatile(pc as *const u16);
        serial_write_str(serial, "instr=0x", usb_dev);
        let mut hx = [0u8; 2];
        hex_u8((instr >> 8) as u8, &mut hx);
        serial_write_all(serial, &hx, usb_dev);
        hex_u8(instr as u8, &mut hx);
        serial_write_all(serial, &hx, usb_dev);
        serial_write_str(serial, "\r\n", usb_dev);

        // Debug command loop
        serial_write_str(serial, "dbg> ", usb_dev);
        let mut shell = ShellState::new();

        loop {
            if !usb_dev.poll(&mut [serial]) {
                continue;
            }
            let mut buf = [0u8; 64];
            match serial.read(&mut buf) {
                Ok(count) if count > 0 => {
                    for i in 0..count {
                        let ch = buf[i];
                        match ch {
                            b'\r' | b'\n' => {
                                serial_write_str(serial, "\r\n", usb_dev);
                                if shell.cmd_len > 0 {
                                    let mut cmd_copy = [0u8; 128];
                                    let len = shell.cmd_len;
                                    cmd_copy[..len].copy_from_slice(&shell.cmd_buf[..len]);
                                    shell.reset();
                                    let cmd = &cmd_copy[..len];

                                    // Debug-specific commands
                                    if cmd == b"cont" || cmd == b"c" {
                                        let demcr = ptr::read_volatile(SCB_DEMCR);
                                        // Clear MON_STEP (18) and MON_PEND (17) — SysTick
                                        // re-pends while we sit at dbg>, must clear it
                                        ptr::write_volatile(SCB_DEMCR, demcr & !((1 << 18) | (1 << 17)));
                                        BUTTON_DEBOUNCE = 0;
                                        serial_write_str(serial, "Continuing...\r\n", usb_dev);
                                        return;
                                    } else if cmd == b"stop" {
                                        serial_write_str(serial, "Stopping app, rebooting to monitor...\r\n", usb_dev);
                                        for _ in 0..50_000 {
                                            usb_dev.poll(&mut [serial]);
                                        }
                                        cortex_m::peripheral::SCB::sys_reset();
                                    } else if cmd == b"step" || cmd == b"s" {
                                        let demcr = ptr::read_volatile(SCB_DEMCR);
                                        ptr::write_volatile(SCB_DEMCR, demcr | (1 << 18));
                                        return; // next instruction will re-enter handler
                                    } else if cmd == b"regs" {
                                        dbg_print_regs(serial, usb_dev, exception_frame);
                                    } else if cmd.starts_with(b"set ") && cmd.len() > 4 {
                                        dbg_set_reg(serial, usb_dev, exception_frame, &cmd[4..]);
                                    } else {
                                        dispatch_command(
                                            cmd, serial, usb_dev, &mut BP_ADDRS,
                                        );
                                    }
                                }
                                serial_write_str(serial, "dbg> ", usb_dev);
                            }
                            0x7F | 0x08 => {
                                if shell.cmd_len > 0 {
                                    shell.cmd_len -= 1;
                                    serial_write_str(serial, "\x08 \x08", usb_dev);
                                }
                            }
                            0x03 => {
                                serial_write_str(serial, "^C\r\ndbg> ", usb_dev);
                                shell.reset();
                            }
                            0x20..=0x7E => {
                                if shell.cmd_len < shell.cmd_buf.len() - 1 {
                                    shell.cmd_buf[shell.cmd_len] = ch;
                                    shell.cmd_len += 1;
                                    serial_write_all(serial, &[ch], usb_dev);
                                }
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }
    }
}

/// Print all registers from exception frame
fn dbg_print_regs(
    serial: &mut SerialPort<'_, UsbBus<UsbFs>>,
    usb_dev: &mut UsbDevice<'_, UsbBus<UsbFs>>,
    frame: *mut u32,
) {
    unsafe {
        let names: [&str; 8] = [" r0", " r1", " r2", " r3", "r12", " lr", " pc", "xpsr"];
        for i in 0..8 {
            serial_write_str(serial, names[i], usb_dev);
            serial_write_str(serial, "=0x", usb_dev);
            serial_write_hex32(serial, ptr::read_volatile(frame.add(i)), usb_dev);
            if i == 3 {
                serial_write_str(serial, "\r\n", usb_dev);
            } else if i < 7 {
                serial_write_str(serial, " ", usb_dev);
            }
        }
        serial_write_str(serial, "\r\n", usb_dev);
    }
}

/// Modify a register in the exception frame: "set r0 deadbeef"
fn dbg_set_reg(
    serial: &mut SerialPort<'_, UsbBus<UsbFs>>,
    usb_dev: &mut UsbDevice<'_, UsbBus<UsbFs>>,
    frame: *mut u32,
    args: &[u8],
) {
    // Parse "rN value"
    let mut parts = args.splitn(2, |&b| b == b' ');
    let reg_name = match parts.next() { Some(r) => r, None => return };
    let val_str = match parts.next() { Some(v) => v, None => {
        serial_write_str(serial, "Usage: set <reg> <hex_val>\r\n", usb_dev);
        return;
    }};
    let val = match parse_hex(val_str) {
        Some(v) => v,
        None => { serial_write_str(serial, "Bad value\r\n", usb_dev); return; }
    };
    let idx = match reg_name {
        b"r0" => Some(0), b"r1" => Some(1), b"r2" => Some(2), b"r3" => Some(3),
        b"r12" => Some(4), b"lr" => Some(5), b"pc" => Some(6), b"xpsr" => Some(7),
        _ => None,
    };
    match idx {
        Some(i) => unsafe {
            ptr::write_volatile(frame.add(i), val);
            serial_write_str(serial, "OK\r\n", usb_dev);
        },
        None => serial_write_str(serial, "Unknown reg (r0-r3,r12,lr,pc,xpsr)\r\n", usb_dev),
    }
}

// ════════════════════════════════════════════════════════════════
// Motor diagnostics command
// ════════════════════════════════════════════════════════════════

fn cmd_motors(serial: &mut SerialPort<'_, UsbBus<UsbFs>>,
              usb_dev: &mut UsbDevice<'_, UsbBus<UsbFs>>) {
    unsafe {
        let gpioe: u32 = 0x4002_1000;
        let tim1: u32 = 0x4001_0000;

        serial_write_str(serial, "=== Motor Diagnostics ===\r\n", usb_dev);
        serial_write_str(serial, "A: PE9/PE11 (TIM1 CH1/CH2)\r\n", usb_dev);
        serial_write_str(serial, "B: PE13/PE14 (TIM1 CH3/CH4)\r\n\r\n", usb_dev);

        // GPIOE MODER — decode motor pin modes
        let moder = ptr::read_volatile((gpioe) as *const u32);
        serial_write_str(serial, "GPIOE_MODER=0x", usb_dev);
        serial_write_hex32(serial, moder, usb_dev);
        serial_write_str(serial, "\r\n", usb_dev);
        let pins: [(u32, &str); 4] = [(9, "PE9 "), (11, "PE11"), (13, "PE13"), (14, "PE14")];
        let modes: [&str; 4] = ["In ", "Out", "AF ", "Ana"];
        for &(pin, name) in &pins {
            let m = ((moder >> (pin * 2)) & 3) as usize;
            serial_write_str(serial, "  ", usb_dev);
            serial_write_str(serial, name, usb_dev);
            serial_write_str(serial, "=", usb_dev);
            serial_write_str(serial, modes[m], usb_dev);
            serial_write_str(serial, "\r\n", usb_dev);
        }

        // GPIOE AFRH — check alternate function assignment
        let afrh = ptr::read_volatile((gpioe + 0x24) as *const u32);
        serial_write_str(serial, "GPIOE_AFRH =0x", usb_dev);
        serial_write_hex32(serial, afrh, usb_dev);
        serial_write_str(serial, "\r\n", usb_dev);
        let af_shifts: [(u32, &str); 4] = [(4, "PE9 "), (12, "PE11"), (20, "PE13"), (24, "PE14")];
        for &(shift, name) in &af_shifts {
            let af = ((afrh >> shift) & 0xF) as u8;
            serial_write_str(serial, "  ", usb_dev);
            serial_write_str(serial, name, usb_dev);
            serial_write_str(serial, "=AF", usb_dev);
            serial_write_all(serial, &[b'0' + af], usb_dev);
            serial_write_str(serial, "\r\n", usb_dev);
        }

        // TIM1 key registers
        serial_write_str(serial, "\r\nTIM1:\r\n", usb_dev);
        let regs: [(&str, u32); 7] = [
            ("CR1  ", 0x00), ("BDTR ", 0x44), ("CCER ", 0x20),
            ("CCMR1", 0x18), ("CCMR2", 0x1C), ("ARR  ", 0x2C), ("PSC  ", 0x28),
        ];
        for &(name, off) in &regs {
            serial_write_str(serial, "  ", usb_dev);
            serial_write_str(serial, name, usb_dev);
            serial_write_str(serial, "=0x", usb_dev);
            serial_write_hex32(serial, ptr::read_volatile((tim1 + off) as *const u32), usb_dev);
            serial_write_str(serial, "\r\n", usb_dev);
        }

        // Compare values (duty cycle)
        serial_write_str(serial, "  CCR1=", usb_dev);
        serial_write_hex32(serial, ptr::read_volatile((tim1 + 0x34) as *const u32), usb_dev);
        serial_write_str(serial, " CCR2=", usb_dev);
        serial_write_hex32(serial, ptr::read_volatile((tim1 + 0x38) as *const u32), usb_dev);
        serial_write_str(serial, "  (Port A)\r\n", usb_dev);
        serial_write_str(serial, "  CCR3=", usb_dev);
        serial_write_hex32(serial, ptr::read_volatile((tim1 + 0x3C) as *const u32), usb_dev);
        serial_write_str(serial, " CCR4=", usb_dev);
        serial_write_hex32(serial, ptr::read_volatile((tim1 + 0x40) as *const u32), usb_dev);
        serial_write_str(serial, "  (Port B)\r\n", usb_dev);

        // Key flags
        let cr1 = ptr::read_volatile((tim1) as *const u32);
        let bdtr = ptr::read_volatile((tim1 + 0x44) as *const u32);
        let ccer = ptr::read_volatile((tim1 + 0x20) as *const u32);
        serial_write_str(serial, "\r\n  CEN=", usb_dev);
        serial_write_all(serial, &[if cr1 & 1 != 0 { b'1' } else { b'0' }], usb_dev);
        serial_write_str(serial, " MOE=", usb_dev);
        serial_write_all(serial, &[if bdtr & (1 << 15) != 0 { b'1' } else { b'0' }], usb_dev);
        serial_write_str(serial, " CH1=", usb_dev);
        serial_write_all(serial, &[if ccer & 1 != 0 { b'1' } else { b'0' }], usb_dev);
        serial_write_str(serial, " CH2=", usb_dev);
        serial_write_all(serial, &[if ccer & (1 << 4) != 0 { b'1' } else { b'0' }], usb_dev);
        serial_write_str(serial, " CH3=", usb_dev);
        serial_write_all(serial, &[if ccer & (1 << 8) != 0 { b'1' } else { b'0' }], usb_dev);
        serial_write_str(serial, " CH4=", usb_dev);
        serial_write_all(serial, &[if ccer & (1 << 12) != 0 { b'1' } else { b'0' }], usb_dev);
        serial_write_str(serial, "\r\n", usb_dev);
    }
}

fn cmd_watch(serial: &mut SerialPort<'_, UsbBus<UsbFs>>,
             usb_dev: &mut UsbDevice<'_, UsbBus<UsbFs>>,
             args: &[&[u8]], watch_addrs: &mut [u32; DWT_MAX_COMP]) {
    if args.is_empty() {
        serial_write_str(serial, "Usage: watch <addr> | watch clear [id] | watch list\r\n", usb_dev);
        return;
    }
    if args[0] == b"list" {
        for i in 0..DWT_MAX_COMP {
            serial_write_str(serial, "  [", usb_dev);
            serial_write_all(serial, &[b'0' + i as u8], usb_dev);
            serial_write_str(serial, "] ", usb_dev);
            if watch_addrs[i] != 0 {
                serial_write_str(serial, "0x", usb_dev);
                serial_write_hex32(serial, watch_addrs[i], usb_dev);
            } else {
                serial_write_str(serial, "empty", usb_dev);
            }
            serial_write_str(serial, "\r\n", usb_dev);
        }
        return;
    }
    if args[0] == b"clear" {
        if args.len() > 1 {
            if let Some(id) = parse_hex(args[1]) {
                if (id as usize) < DWT_MAX_COMP {
                    unsafe { dwt_clear_watchpoint(id as usize); }
                    watch_addrs[id as usize] = 0;
                    serial_write_str(serial, "Watch cleared\r\n", usb_dev);
                }
            }
        } else {
            for i in 0..DWT_MAX_COMP {
                unsafe { dwt_clear_watchpoint(i); }
            }
            *watch_addrs = [0; DWT_MAX_COMP];
            serial_write_str(serial, "All watches cleared\r\n", usb_dev);
        }
        return;
    }
    // watch <addr> — find free slot
    let addr = match parse_hex(args[0]) {
        Some(a) => a,
        None => { serial_write_str(serial, "Bad address\r\n", usb_dev); return; }
    };
    for i in 0..DWT_MAX_COMP {
        if watch_addrs[i] == 0 {
            if unsafe { dwt_set_watchpoint(i, addr, true) } {
                watch_addrs[i] = addr;
                serial_write_str(serial, "Watch [", usb_dev);
                serial_write_all(serial, &[b'0' + i as u8], usb_dev);
                serial_write_str(serial, "] on write to 0x", usb_dev);
                serial_write_hex32(serial, addr, usb_dev);
                serial_write_str(serial, "\r\n", usb_dev);
            }
            return;
        }
    }
    serial_write_str(serial, "No free watch slots\r\n", usb_dev);
}

// ════════════════════════════════════════════════════════════════
// Text I/O helpers for the serial shell
// ════════════════════════════════════════════════════════════════

struct ShellState {
    cmd_buf: [u8; 128],
    cmd_len: usize,
}

impl ShellState {
    const fn new() -> Self {
        Self {
            cmd_buf: [0u8; 128],
            cmd_len: 0,
        }
    }

    fn reset(&mut self) {
        self.cmd_len = 0;
    }
}

/// Hex formatting helpers (no alloc needed)
fn hex_u32(val: u32, buf: &mut [u8; 8]) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for i in 0..8 {
        buf[7 - i] = HEX[((val >> (i * 4)) & 0xF) as usize];
    }
}

fn hex_u8(val: u8, buf: &mut [u8; 2]) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    buf[0] = HEX[(val >> 4) as usize];
    buf[1] = HEX[(val & 0xF) as usize];
}

/// Parse a hex string to u32
fn parse_hex(s: &[u8]) -> Option<u32> {
    if s.is_empty() || s.len() > 8 {
        return None;
    }
    // Skip optional "0x" prefix
    let s = if s.len() >= 2 && s[0] == b'0' && (s[1] == b'x' || s[1] == b'X') {
        &s[2..]
    } else {
        s
    };
    if s.is_empty() {
        return None;
    }
    let mut val = 0u32;
    for &ch in s {
        let digit = match ch {
            b'0'..=b'9' => ch - b'0',
            b'a'..=b'f' => ch - b'a' + 10,
            b'A'..=b'F' => ch - b'A' + 10,
            _ => return None,
        };
        val = val.checked_shl(4)?.checked_add(digit as u32)?;
    }
    Some(val)
}

// ════════════════════════════════════════════════════════════════
// Serial output helpers
// ════════════════════════════════════════════════════════════════

/// Write a byte slice to USB serial, retrying until all bytes sent.
fn serial_write_all(serial: &mut SerialPort<'_, UsbBus<UsbFs>>, data: &[u8],
                    usb_dev: &mut UsbDevice<'_, UsbBus<UsbFs>>) {
    let mut offset = 0;
    let mut retries = 0u32;
    while offset < data.len() {
        match serial.write(&data[offset..]) {
            Ok(n) => {
                offset += n;
                retries = 0;
            }
            Err(UsbError::WouldBlock) => {
                usb_dev.poll(&mut [serial]);
                retries += 1;
                if retries > 100_000 {
                    return; // give up
                }
            }
            Err(_) => return,
        }
    }
}

fn serial_write_str(serial: &mut SerialPort<'_, UsbBus<UsbFs>>, s: &str,
                    usb_dev: &mut UsbDevice<'_, UsbBus<UsbFs>>) {
    serial_write_all(serial, s.as_bytes(), usb_dev);
}

fn serial_write_hex32(serial: &mut SerialPort<'_, UsbBus<UsbFs>>, val: u32,
                      usb_dev: &mut UsbDevice<'_, UsbBus<UsbFs>>) {
    let mut buf = [0u8; 8];
    hex_u32(val, &mut buf);
    serial_write_all(serial, &buf, usb_dev);
}

// ════════════════════════════════════════════════════════════════
// Command handlers
// ════════════════════════════════════════════════════════════════

fn cmd_help(serial: &mut SerialPort<'_, UsbBus<UsbFs>>,
            usb_dev: &mut UsbDevice<'_, UsbBus<UsbFs>>) {
    serial_write_str(serial, concat!(
        "LEGO Hub Monitor v0.2\r\n",
        "Commands:\r\n",
        "  help              Show this help\r\n",
        "  peek <addr>       Read 32-bit word\r\n",
        "  peek16 <addr>     Read 16-bit half-word\r\n",
        "  peek8 <addr>      Read byte\r\n",
        "  poke <addr> <val> Write 32-bit word\r\n",
        "  dump <addr> <len> Hex dump (max 256 bytes)\r\n",
        "  bp <addr>         Set HW breakpoint (FPB, max 6)\r\n",
        "  bp clear [id]     Clear breakpoint(s)\r\n",
        "  bp list           List active breakpoints\r\n",
        "  watch <addr>      Set data watchpoint (DWT, max 4)\r\n",
        "  watch clear [id]  Clear watchpoint(s)\r\n",
        "  watch list        List active watchpoints\r\n",
        "  motors            Dump motor GPIO/TIM1 config\r\n",
        "  dbgmon            Enable DebugMonitor exception\r\n",
        "  upload            Flash app binary (serial)\r\n",
        "  run               Arm monitor + jump to app\r\n",
        "                    (center button = pause)\r\n",
        "  dfu               Enter STM32 system DFU\r\n",
        "  reboot            Reset MCU\r\n",
        "In breakpoint (dbg>):\r\n",
        "  cont / c          Continue execution\r\n",
        "  stop              Stop app, reboot to monitor\r\n",
        "  step / s          Single-step one instruction\r\n",
        "  regs              Show saved registers\r\n",
        "  set <reg> <val>   Modify register (r0-r3,r12,lr,pc)\r\n",
    ), usb_dev);
}

fn cmd_peek(serial: &mut SerialPort<'_, UsbBus<UsbFs>>,
            usb_dev: &mut UsbDevice<'_, UsbBus<UsbFs>>,
            args: &[&[u8]], width: u32) {
    if args.is_empty() {
        serial_write_str(serial, "Usage: peek <hex_addr>\r\n", usb_dev);
        return;
    }
    let addr = match parse_hex(args[0]) {
        Some(a) => a,
        None => {
            serial_write_str(serial, "Bad address\r\n", usb_dev);
            return;
        }
    };

    // Alignment check
    match width {
        4 => if addr & 3 != 0 {
            serial_write_str(serial, "Address not 4-byte aligned\r\n", usb_dev);
            return;
        },
        2 => if addr & 1 != 0 {
            serial_write_str(serial, "Address not 2-byte aligned\r\n", usb_dev);
            return;
        },
        _ => {}
    }

    serial_write_str(serial, "0x", usb_dev);
    serial_write_hex32(serial, addr, usb_dev);
    serial_write_str(serial, " = 0x", usb_dev);

    unsafe {
        match width {
            4 => serial_write_hex32(serial, ptr::read_volatile(addr as *const u32), usb_dev),
            2 => {
                let val = ptr::read_volatile(addr as *const u16) as u32;
                let mut buf = [0u8; 4];
                let (hi, lo) = buf.split_at_mut(2);
                hex_u8((val >> 8) as u8, hi.try_into().unwrap());
                hex_u8(val as u8, lo.try_into().unwrap());
                serial_write_all(serial, &buf, usb_dev);
            }
            _ => {
                let val = ptr::read_volatile(addr as *const u8);
                let mut buf = [0u8; 2];
                hex_u8(val, &mut buf);
                serial_write_all(serial, &buf, usb_dev);
            }
        }
    }
    serial_write_str(serial, "\r\n", usb_dev);
}

fn cmd_poke(serial: &mut SerialPort<'_, UsbBus<UsbFs>>,
            usb_dev: &mut UsbDevice<'_, UsbBus<UsbFs>>,
            args: &[&[u8]]) {
    if args.len() < 2 {
        serial_write_str(serial, "Usage: poke <hex_addr> <hex_val>\r\n", usb_dev);
        return;
    }
    let addr = match parse_hex(args[0]) {
        Some(a) => a,
        None => { serial_write_str(serial, "Bad address\r\n", usb_dev); return; }
    };
    let val = match parse_hex(args[1]) {
        Some(v) => v,
        None => { serial_write_str(serial, "Bad value\r\n", usb_dev); return; }
    };
    if addr & 3 != 0 {
        serial_write_str(serial, "Address not 4-byte aligned\r\n", usb_dev);
        return;
    }
    unsafe { ptr::write_volatile(addr as *mut u32, val); }
    serial_write_str(serial, "OK\r\n", usb_dev);
}

fn cmd_dump(serial: &mut SerialPort<'_, UsbBus<UsbFs>>,
            usb_dev: &mut UsbDevice<'_, UsbBus<UsbFs>>,
            args: &[&[u8]]) {
    if args.len() < 2 {
        serial_write_str(serial, "Usage: dump <hex_addr> <hex_len>\r\n", usb_dev);
        return;
    }
    let addr = match parse_hex(args[0]) {
        Some(a) => a,
        None => { serial_write_str(serial, "Bad address\r\n", usb_dev); return; }
    };
    let len = match parse_hex(args[1]) {
        Some(l) => l.min(256),
        None => { serial_write_str(serial, "Bad length\r\n", usb_dev); return; }
    };

    let mut offset = 0u32;
    while offset < len {
        // Address prefix
        serial_write_str(serial, "0x", usb_dev);
        serial_write_hex32(serial, addr + offset, usb_dev);
        serial_write_str(serial, ": ", usb_dev);

        // 16 bytes per line
        let line_end = (offset + 16).min(len);
        for i in offset..line_end {
            let byte = unsafe { ptr::read_volatile((addr + i) as *const u8) };
            let mut hx = [0u8; 2];
            hex_u8(byte, &mut hx);
            serial_write_all(serial, &hx, usb_dev);
            serial_write_str(serial, " ", usb_dev);
        }

        // ASCII
        serial_write_str(serial, " |", usb_dev);
        for i in offset..line_end {
            let byte = unsafe { ptr::read_volatile((addr + i) as *const u8) };
            let ch = if (0x20..=0x7E).contains(&byte) { byte } else { b'.' };
            serial_write_all(serial, &[ch], usb_dev);
        }
        serial_write_str(serial, "|\r\n", usb_dev);

        offset = line_end;
    }
}

fn cmd_bp(serial: &mut SerialPort<'_, UsbBus<UsbFs>>,
          usb_dev: &mut UsbDevice<'_, UsbBus<UsbFs>>,
          args: &[&[u8]], bp_addrs: &mut [u32; FPB_MAX_COMP]) {
    if args.is_empty() {
        serial_write_str(serial, "Usage: bp <addr> | bp clear [id] | bp list\r\n", usb_dev);
        return;
    }

    if args[0] == b"list" {
        for i in 0..FPB_MAX_COMP {
            serial_write_str(serial, "  [", usb_dev);
            serial_write_all(serial, &[b'0' + i as u8], usb_dev);
            serial_write_str(serial, "] ", usb_dev);
            if bp_addrs[i] != 0 {
                serial_write_str(serial, "0x", usb_dev);
                serial_write_hex32(serial, bp_addrs[i], usb_dev);
            } else {
                serial_write_str(serial, "empty", usb_dev);
            }
            serial_write_str(serial, "\r\n", usb_dev);
        }
        return;
    }

    if args[0] == b"clear" {
        if args.len() > 1 {
            if let Some(id) = parse_hex(args[1]) {
                if (id as usize) < FPB_MAX_COMP {
                    unsafe { fpb_clear_breakpoint(id as usize); }
                    bp_addrs[id as usize] = 0;
                    serial_write_str(serial, "BP cleared\r\n", usb_dev);
                } else {
                    serial_write_str(serial, "Invalid BP id\r\n", usb_dev);
                }
            }
        } else {
            unsafe { fpb_clear_all(); }
            *bp_addrs = [0; FPB_MAX_COMP];
            serial_write_str(serial, "All BPs cleared\r\n", usb_dev);
        }
        return;
    }

    // bp <addr> — find free slot
    let addr = match parse_hex(args[0]) {
        Some(a) => a,
        None => { serial_write_str(serial, "Bad address\r\n", usb_dev); return; }
    };

    if addr >= 0x2000_0000 {
        serial_write_str(serial, "FPB only supports code region (<0x20000000)\r\n", usb_dev);
        return;
    }

    for i in 0..FPB_MAX_COMP {
        if bp_addrs[i] == 0 {
            if unsafe { fpb_set_breakpoint(i, addr) } {
                bp_addrs[i] = addr;
                serial_write_str(serial, "BP [", usb_dev);
                serial_write_all(serial, &[b'0' + i as u8], usb_dev);
                serial_write_str(serial, "] set at 0x", usb_dev);
                serial_write_hex32(serial, addr, usb_dev);
                serial_write_str(serial, "\r\n", usb_dev);
            } else {
                serial_write_str(serial, "Failed to set BP\r\n", usb_dev);
            }
            return;
        }
    }
    serial_write_str(serial, "No free BP slots\r\n", usb_dev);
}

fn cmd_dbgmon(serial: &mut SerialPort<'_, UsbBus<UsbFs>>,
              usb_dev: &mut UsbDevice<'_, UsbBus<UsbFs>>) {
    let ok = unsafe { debug_monitor_enable() };
    if ok {
        serial_write_str(serial, "DebugMonitor enabled (priority 0xFF)\r\n", usb_dev);
    } else {
        serial_write_str(serial, "Cannot enable: halting debug is active\r\n", usb_dev);
    }
}

fn cmd_upload(serial: &mut SerialPort<'_, UsbBus<UsbFs>>,
              usb_dev: &mut UsbDevice<'_, UsbBus<UsbFs>>) {
    // Unlock flash
    unsafe {
        reg_write(FLASH_R, 0x04, 0x4567_0123);
        reg_write(FLASH_R, 0x04, 0xCDEF_89AB);
    }

    // Erase sector 4 (64KB at 0x08010000)
    serial_write_str(serial, "Erasing sector 4...", usb_dev);
    unsafe {
        while reg_read(FLASH_R, 0x0C) & (1 << 16) != 0 {}
        // SER=1, SNB=4, PSIZE=word, STRT=1
        reg_write(FLASH_R, 0x10, (1 << 1) | (4 << 3) | (2 << 8) | (1 << 16));
        while reg_read(FLASH_R, 0x0C) & (1 << 16) != 0 {}
        reg_write(FLASH_R, 0x10, 0);
    }
    serial_write_str(serial, " OK\r\nSend: ADDR VALUE (hex), 'end' to finish.\r\n", usb_dev);

    // Enable flash programming: PG=1, PSIZE=word
    unsafe { reg_write(FLASH_R, 0x10, (1 << 0) | (2 << 8)); }

    let mut total: u32 = 0;
    let mut line = [0u8; 32];
    let mut len: usize = 0;

    loop {
        if !usb_dev.poll(&mut [serial]) { continue; }
        let mut buf = [0u8; 64];
        let count = match serial.read(&mut buf) {
            Ok(n) if n > 0 => n,
            _ => continue,
        };

        for i in 0..count {
            match buf[i] {
                b'\r' | b'\n' => {
                    if len == 0 { continue; }
                    if len == 3 && line[0] == b'e' && line[1] == b'n' && line[2] == b'd' {
                        // Finish: clear PG, lock flash
                        unsafe {
                            reg_write(FLASH_R, 0x10, 0);
                            reg_modify(FLASH_R, 0x10, 0, 1 << 31);
                        }
                        serial_write_str(serial, "OK ", usb_dev);
                        serial_write_hex32(serial, total, usb_dev);
                        serial_write_str(serial, " bytes\r\n", usb_dev);
                        return;
                    }
                    // Parse "ADDR VALUE"
                    if let Some(sp) = line[..len].iter().position(|&c| c == b' ') {
                        if let (Some(addr), Some(val)) = (
                            parse_hex(&line[..sp]),
                            parse_hex(&line[sp + 1..len]),
                        ) {
                            if addr >= 0x0801_0000 && addr < 0x0802_0000 && addr & 3 == 0 {
                                unsafe {
                                    while reg_read(FLASH_R, 0x0C) & (1 << 16) != 0 {}
                                    ptr::write_volatile(addr as *mut u32, val);
                                    while reg_read(FLASH_R, 0x0C) & (1 << 16) != 0 {}
                                }
                                total += 4;
                            }
                        }
                    }
                    len = 0;
                }
                _ => {
                    if len < 32 { line[len] = buf[i]; len += 1; }
                }
            }
        }
    }
}

// ════════════════════════════════════════════════════════════════
// Command dispatcher
// ════════════════════════════════════════════════════════════════

fn dispatch_command(
    cmd_buf: &[u8],
    serial: &mut SerialPort<'_, UsbBus<UsbFs>>,
    usb_dev: &mut UsbDevice<'_, UsbBus<UsbFs>>,
    bp_addrs: &mut [u32; FPB_MAX_COMP],
) -> bool {
    // Split into words
    let mut words: [&[u8]; 4] = [&[]; 4];
    let mut word_count = 0;
    let mut in_word = false;
    let mut start = 0;

    for (i, &ch) in cmd_buf.iter().enumerate() {
        if ch == b' ' || ch == b'\t' {
            if in_word && word_count < 4 {
                words[word_count] = &cmd_buf[start..i];
                word_count += 1;
                in_word = false;
            }
        } else if !in_word {
            start = i;
            in_word = true;
        }
    }
    if in_word && word_count < 4 {
        words[word_count] = &cmd_buf[start..];
        word_count += 1;
    }

    if word_count == 0 {
        return false;
    }

    let cmd = words[0];
    let args = &words[1..word_count];

    match cmd {
        b"help" | b"?" => cmd_help(serial, usb_dev),
        b"peek" => cmd_peek(serial, usb_dev, args, 4),
        b"peek16" => cmd_peek(serial, usb_dev, args, 2),
        b"peek8" => cmd_peek(serial, usb_dev, args, 1),
        b"poke" => cmd_poke(serial, usb_dev, args),
        b"dump" => cmd_dump(serial, usb_dev, args),
        b"bp" => cmd_bp(serial, usb_dev, args, bp_addrs),
        b"watch" => unsafe { cmd_watch(serial, usb_dev, args, &mut WATCH_ADDRS) },
        b"motors" => cmd_motors(serial, usb_dev),
        b"dbgmon" => cmd_dbgmon(serial, usb_dev),
        b"upload" => cmd_upload(serial, usb_dev),
        b"run" => {
            let app_sp = unsafe { ptr::read_volatile(APP_ADDR as *const u32) };
            let app_pc = unsafe { ptr::read_volatile((APP_ADDR + 4) as *const u32) };
            let sp_valid = app_sp >= 0x2000_0000 && app_sp <= 0x2005_0000;
            let pc_valid = app_pc >= APP_ADDR && app_pc <= 0x0810_0000;
            if sp_valid && pc_valid {
                // Arm DebugMonitor before launching app
                unsafe {
                    debug_monitor_enable();
                    // Write our handler address to trampoline location
                    // The app's DebugMonitor stub reads this and jumps to us
                    extern "C" { fn DebugMonitor(); }
                    ptr::write_volatile(TRAMPOLINE_ADDR, DebugMonitor as *const () as u32);
                }
                serial_write_str(serial, "Monitor armed. Center button = pause.\r\n", usb_dev);
                serial_write_str(serial, "Jumping to app...\r\n", usb_dev);
                for _ in 0..50_000 {
                    usb_dev.poll(&mut [serial]);
                }
                unsafe {
                    arm_systick();
                    jump_to_app(app_sp, app_pc);
                }
            } else {
                serial_write_str(serial, "No valid app at 0x08010000\r\n", usb_dev);
                serial_write_str(serial, "  SP=0x", usb_dev);
                serial_write_hex32(serial, app_sp, usb_dev);
                serial_write_str(serial, " PC=0x", usb_dev);
                serial_write_hex32(serial, app_pc, usb_dev);
                serial_write_str(serial, "\r\n", usb_dev);
            }
        }
        b"dfu" => {
            serial_write_str(serial, "Entering DFU...\r\n", usb_dev);
            for _ in 0..50_000 {
                usb_dev.poll(&mut [serial]);
            }
            unsafe {
                ptr::write_volatile(DFU_MAGIC_ADDR, DFU_MAGIC_VALUE);
            }
            cortex_m::peripheral::SCB::sys_reset();
        }
        b"reboot" => {
            serial_write_str(serial, "Rebooting...\r\n", usb_dev);
            for _ in 0..50_000 {
                usb_dev.poll(&mut [serial]);
            }
            cortex_m::peripheral::SCB::sys_reset();
        }
        _ => {
            serial_write_str(serial, "Unknown: ", usb_dev);
            serial_write_all(serial, cmd, usb_dev);
            serial_write_str(serial, " (type 'help')\r\n", usb_dev);
        }
    }
    false
}

// ════════════════════════════════════════════════════════════════
// Entry point
// ════════════════════════════════════════════════════════════════

static mut EP_MEMORY: [u32; 512] = [0; 512];

#[cortex_m_rt::entry]
fn main() -> ! {
    unsafe {
        // ── 1. Check DFU magic ──────────────────────────────
        let magic = ptr::read_volatile(DFU_MAGIC_ADDR);
        if magic == DFU_MAGIC_VALUE {
            ptr::write_volatile(DFU_MAGIC_ADDR, 0);
            enter_system_dfu();
        }

        // ── 2. Init clocks (96 MHz + 48 MHz USB) ────────────
        init_clocks();

        // ── 3. Alive indicator: beep + status LED green ─────
        beep(150, 1000);  // 150ms @ 1kHz
        init_status_led();

        // ── 4. Init USB pins ────────────────────────────────
        init_usb_pins();

        // ── 4. Set up USB CDC serial (global statics) ───────
        let usb_periph = UsbFs { _private: () };
        USB_BUS_G = Some(UsbBus::new(usb_periph, &mut EP_MEMORY));
        let usb_bus = USB_BUS_G.as_ref().unwrap();

        USB_SERIAL = Some(SerialPort::new(usb_bus));
        USB_DEVICE = Some(
            UsbDeviceBuilder::new(usb_bus, UsbVidPid(0x1209, 0x0001))
                .strings(&[StringDescriptors::default()
                    .manufacturer("LegoHubMonitor")
                    .product("Debug Monitor")
                    .serial_number("0001")])
                .unwrap()
                .device_class(usbd_serial::USB_CLASS_CDC)
                .max_packet_size_0(64)
                .unwrap()
                .build(),
        );

        let serial = USB_SERIAL.as_mut().unwrap();
        let usb_dev = USB_DEVICE.as_mut().unwrap();

        // ── 5. Poll USB forever — no timeout, no auto-jump ─
        // Hub stays alive (PA13 HIGH). Just keep polling until
        // USB host connects. Safe even if no cable plugged in.

        // ── 6. Interactive monitor shell ────────────────────
        let mut shell = ShellState::new();

        serial_write_str(serial, "\r\n╔══════════════════════════════════╗\r\n", usb_dev);
        serial_write_str(serial, "║  LEGO Hub Debug Monitor v0.2    ║\r\n", usb_dev);
        serial_write_str(serial, "║  STM32F413 @ 96 MHz            ║\r\n", usb_dev);
        serial_write_str(serial, "║  Type 'help' for commands      ║\r\n", usb_dev);
        serial_write_str(serial, "╚══════════════════════════════════╝\r\n", usb_dev);
        serial_write_str(serial, "> ", usb_dev);

        loop {
            if !usb_dev.poll(&mut [serial]) {
                continue;
            }

            let mut buf = [0u8; 64];
            match serial.read(&mut buf) {
                Ok(count) if count > 0 => {
                    for i in 0..count {
                        let ch = buf[i];
                        match ch {
                            // Enter (CR or LF)
                            b'\r' | b'\n' => {
                                serial_write_str(serial, "\r\n", usb_dev);
                                if shell.cmd_len > 0 {
                                    let cmd_slice = &shell.cmd_buf[..shell.cmd_len];
                                    // Copy to stack buffer to avoid borrow issues
                                    let mut cmd_copy = [0u8; 128];
                                    let len = shell.cmd_len;
                                    cmd_copy[..len].copy_from_slice(cmd_slice);
                                    shell.reset();
                                    dispatch_command(
                                        &cmd_copy[..len],
                                        serial,
                                        usb_dev,
                                        &mut BP_ADDRS,
                                    );
                                }
                                serial_write_str(serial, "> ", usb_dev);
                            }
                            // Backspace
                            0x7F | 0x08 => {
                                if shell.cmd_len > 0 {
                                    shell.cmd_len -= 1;
                                    serial_write_str(serial, "\x08 \x08", usb_dev);
                                }
                            }
                            // Ctrl+C
                            0x03 => {
                                serial_write_str(serial, "^C\r\n> ", usb_dev);
                                shell.reset();
                            }
                            // Printable characters
                            0x20..=0x7E => {
                                if shell.cmd_len < shell.cmd_buf.len() - 1 {
                                    shell.cmd_buf[shell.cmd_len] = ch;
                                    shell.cmd_len += 1;
                                    serial_write_all(serial, &[ch], usb_dev);
                                }
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }
    }
}
