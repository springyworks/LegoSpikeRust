//! RTIC firmware for LEGO SPIKE Prime / Robot Inventor 51515 hub
//!
//! Loaded by our Rust bootloader (0x08008000). This app lives at 0x08010000.
//! Uses RTIC v2, defmt logging, raw register access for motors + LED matrix.
//!
//! Flash layout:
//!   0x08000000  LEGO DFU bootloader  (32 KB, factory)
//!   0x08008000  Rust bootloader      (32 KB, flashed once via LEGO DFU)
//!   0x08010000  This application     (960 KB, flashed via STM32 system DFU 0483:df11)
//!
//! Dev cycle: edit code → save → dev.sh builds + flashes automatically.
//! The app enters STM32 system DFU after a quick demo when USB is connected.
//!
//! Hardware (STM32F413VGT6 — Cortex-M4F, 96 MHz, 1 MB Flash, 320 KB RAM):
//!   Port A motor H-bridge: PE9  (TIM1_CH1) + PE11 (TIM1_CH2)
//!   Port B motor H-bridge: PE13 (TIM1_CH3) + PE14 (TIM1_CH4)
//!   PA13: power hold (LOW = power off!)
//!   PA14: I/O port VCC enable
//!   LED matrix: TLC5955 on SPI1 (PA5/PA6/PA7), LAT=PA15, GSCLK=PB15 (TIM12_CH2)
//!   Center button: PC4, ADC1 channel 14 (resistor ladder, ≤2879 = pressed)

#![no_std]
#![no_main]

use defmt_rtt as _;
use panic_probe as _;

// ════════════════════════════════════════════════════════════════
// DebugMonitor trampoline — forwards to monitor's handler
// The monitor writes its handler address to 0x2004_FFE0 before
// launching the app. This stub reads that address and jumps there,
// so breakpoints and single-step work through the resident monitor.
// ════════════════════════════════════════════════════════════════

core::arch::global_asm!(
    ".section .text",
    ".global DebugMonitor",
    ".type DebugMonitor, %function",
    ".thumb_func",
    "DebugMonitor:",
    "ldr r12, =0x2004FFE0",  // trampoline address in RAM
    "ldr r12, [r12]",         // load monitor handler address
    "cmp r12, #0",
    "beq 1f",                 // no handler → return
    "bx r12",                 // jump to monitor (LR = EXC_RETURN, preserved)
    "1:",
    "bx lr",                  // default: return from exception
);

// ════════════════════════════════════════════════════════════════
// Register base addresses (STM32F413 reference manual)
// ════════════════════════════════════════════════════════════════
const RCC: u32 = 0x4002_3800;
const FLASH_R: u32 = 0x4002_3C00;
const GPIOA: u32 = 0x4002_0000;
const GPIOB: u32 = 0x4002_0400;
const GPIOC: u32 = 0x4002_0800;
const GPIOE: u32 = 0x4002_1000;
const SPI1_BASE: u32 = 0x4001_3000;
const ADC1: u32 = 0x4001_2000;
const TIM1: u32 = 0x4001_0000;
const TIM12: u32 = 0x4000_1800;

// RCC register offsets
const RCC_CR: u32 = 0x00;
const RCC_PLLCFGR: u32 = 0x04;
const RCC_CFGR: u32 = 0x08;
const RCC_AHB1ENR: u32 = 0x30;
const RCC_APB1ENR: u32 = 0x40;
const RCC_APB2ENR: u32 = 0x44;

// FLASH
const FLASH_ACR: u32 = 0x00;

// GPIO register offsets
const GPIO_MODER: u32 = 0x00;
const GPIO_OSPEEDR: u32 = 0x08;
const GPIO_IDR: u32 = 0x10;
const GPIO_BSRR: u32 = 0x18;
const GPIO_AFRL: u32 = 0x20;
const GPIO_AFRH: u32 = 0x24;

// TIM register offsets (shared between TIM1 and TIM12)
const TIM_CR1: u32 = 0x00;
const TIM_EGR: u32 = 0x14;
const TIM_CCMR1: u32 = 0x18;
const TIM_CCMR2: u32 = 0x1C;
const TIM_CCER: u32 = 0x20;
const TIM_PSC: u32 = 0x28;
const TIM_ARR: u32 = 0x2C;
const TIM_CCR1: u32 = 0x34;
const TIM_CCR2: u32 = 0x38;
const TIM_CCR3: u32 = 0x3C;
const TIM_CCR4: u32 = 0x40;
const TIM_BDTR: u32 = 0x44;

// SPI register offsets
const SPI_CR1: u32 = 0x00;
const SPI_SR: u32 = 0x08;
const SPI_DR: u32 = 0x0C;

// ADC register offsets
const ADC_SR: u32 = 0x00;
const ADC_CR2: u32 = 0x08;
const ADC_SMPR1: u32 = 0x0C;
const ADC_SMPR2: u32 = 0x10;
const ADC_SQR1: u32 = 0x2C;
const ADC_SQR3: u32 = 0x34;
const ADC_DR: u32 = 0x4C;

// ════════════════════════════════════════════════════════════════
// DFU re-entry: write magic → reset → bootloader catches it
//   → enters STM32 system bootloader DFU (0483:df11)
// ════════════════════════════════════════════════════════════════

const DFU_MAGIC_ADDR: *mut u32 = 0x2004_FFF0 as *mut u32;
const DFU_MAGIC_VALUE: u32 = 0xDEAD_B007;

pub fn request_dfu() -> ! {
    unsafe {
        core::ptr::write_volatile(DFU_MAGIC_ADDR, DFU_MAGIC_VALUE);
    }
    cortex_m::peripheral::SCB::sys_reset();
}

// ════════════════════════════════════════════════════════════════
// Low-level register helpers
// ════════════════════════════════════════════════════════════════

#[inline(always)]
unsafe fn reg_write(base: u32, offset: u32, val: u32) {
    core::ptr::write_volatile((base + offset) as *mut u32, val);
}

#[inline(always)]
unsafe fn reg_read(base: u32, offset: u32) -> u32 {
    core::ptr::read_volatile((base + offset) as *const u32)
}

#[inline(always)]
unsafe fn reg_modify(base: u32, offset: u32, clear: u32, set: u32) {
    let v = reg_read(base, offset);
    reg_write(base, offset, (v & !clear) | set);
}

// ════════════════════════════════════════════════════════════════
// Clock configuration: HSE 16 MHz → PLL → 96 MHz SYSCLK
// ════════════════════════════════════════════════════════════════

unsafe fn init_clocks() {
    reg_modify(FLASH_R, FLASH_ACR, 0xF, 3);
    reg_modify(RCC, RCC_CR, 0, 1 << 16);
    while reg_read(RCC, RCC_CR) & (1 << 17) == 0 {}
    reg_write(
        RCC,
        RCC_PLLCFGR,
        16 | (192 << 6) | (0 << 16) | (1 << 22) | (4 << 24),
    );
    reg_modify(RCC, RCC_CR, 0, 1 << 24);
    while reg_read(RCC, RCC_CR) & (1 << 25) == 0 {}
    reg_modify(RCC, RCC_CFGR, 0xFCF0, 0x1000);
    reg_modify(RCC, RCC_CFGR, 0x3, 0x2);
    while (reg_read(RCC, RCC_CFGR) & 0xC) != 0x8 {}
}

// ════════════════════════════════════════════════════════════════
// Power and GPIO initialization
// ════════════════════════════════════════════════════════════════

unsafe fn init_power_and_gpio() {
    reg_modify(RCC, RCC_AHB1ENR, 0, (1 << 0) | (1 << 4));
    let _ = reg_read(RCC, RCC_AHB1ENR);
    let _ = reg_read(RCC, RCC_AHB1ENR);
    reg_modify(GPIOA, GPIO_MODER, 3 << 26, 1 << 26);
    reg_write(GPIOA, GPIO_BSRR, 1 << 13);
    reg_modify(GPIOA, GPIO_MODER, 3 << 28, 1 << 28);
    reg_write(GPIOA, GPIO_BSRR, 1 << 14);
    let af_clear = (0xF << 4) | (0xF << 12) | (0xF << 20) | (0xF << 24);
    let af_set = (1 << 4) | (1 << 12) | (1 << 20) | (1 << 24);
    reg_modify(GPIOE, GPIO_AFRH, af_clear, af_set);
    let speed_mask = (3 << 18) | (3 << 22) | (3 << 26) | (3 << 28);
    reg_modify(GPIOE, GPIO_OSPEEDR, speed_mask, speed_mask);
    let moder_mask = (3u32 << 18) | (3 << 22) | (3 << 26) | (3 << 28);
    let moder_out = (1u32 << 18) | (1 << 22) | (1 << 26) | (1 << 28);
    reg_modify(GPIOE, GPIO_MODER, moder_mask, moder_out);
    reg_write(GPIOE, GPIO_BSRR, (1 << 25) | (1 << 27) | (1 << 29) | (1 << 30));
}

// ════════════════════════════════════════════════════════════════
// TIM1 setup: 12 kHz PWM on channels 1–4, inverted polarity
// ════════════════════════════════════════════════════════════════

unsafe fn init_timer() {
    reg_modify(RCC, RCC_APB2ENR, 0, 1 << 0);
    let _ = reg_read(RCC, RCC_APB2ENR);
    reg_write(TIM1, TIM_PSC, 7);
    reg_write(TIM1, TIM_ARR, 999);
    reg_write(TIM1, TIM_CCMR1, 0x6868);
    reg_write(TIM1, TIM_CCMR2, 0x6868);
    reg_write(TIM1, TIM_CCER, 0x3333);
    reg_write(TIM1, TIM_CCR1, 0);
    reg_write(TIM1, TIM_CCR2, 0);
    reg_write(TIM1, TIM_CCR3, 0);
    reg_write(TIM1, TIM_CCR4, 0);
    reg_write(TIM1, TIM_BDTR, 1 << 15);
    reg_write(TIM1, TIM_CR1, (1 << 7) | (1 << 0));
    reg_write(TIM1, TIM_EGR, 1);
}

// ════════════════════════════════════════════════════════════════
// Motor control API
// ════════════════════════════════════════════════════════════════

#[derive(Clone, Copy, defmt::Format)]
pub enum Motor {
    A,
    B,
}

impl Motor {
    const fn hw(self) -> (u32, u32, u32, u32, u32, u32) {
        match self {
            Motor::A => (9, 11, 18, 22, TIM_CCR1, TIM_CCR2),
            Motor::B => (13, 14, 26, 28, TIM_CCR3, TIM_CCR4),
        }
    }
}

pub fn motor_set(motor: Motor, duty: i16) {
    let duty = duty.clamp(-1000, 1000);
    let (pin1, pin2, sh1, sh2, ccr1, ccr2) = motor.hw();
    unsafe {
        if duty > 0 {
            reg_write(TIM1, ccr1, duty as u32);
            reg_modify(GPIOE, GPIO_MODER, (3 << sh1) | (3 << sh2), (2 << sh1) | (1 << sh2));
            reg_write(GPIOE, GPIO_BSRR, 1 << pin2);
        } else if duty < 0 {
            reg_write(TIM1, ccr2, (-duty) as u32);
            reg_modify(GPIOE, GPIO_MODER, (3 << sh1) | (3 << sh2), (1 << sh1) | (2 << sh2));
            reg_write(GPIOE, GPIO_BSRR, 1 << pin1);
        } else {
            reg_modify(GPIOE, GPIO_MODER, (3 << sh1) | (3 << sh2), (1 << sh1) | (1 << sh2));
            reg_write(GPIOE, GPIO_BSRR, (1 << pin1) | (1 << pin2));
        }
    }
}

pub fn motor_coast(motor: Motor) {
    let (pin1, pin2, sh1, sh2, _, _) = motor.hw();
    unsafe {
        reg_modify(GPIOE, GPIO_MODER, (3 << sh1) | (3 << sh2), (1 << sh1) | (1 << sh2));
        reg_write(GPIOE, GPIO_BSRR, (1 << (pin1 + 16)) | (1 << (pin2 + 16)));
    }
}

// ════════════════════════════════════════════════════════════════
// Timing / VBUS detection
// ════════════════════════════════════════════════════════════════

/// Busy-wait delay — used in init (before monotonic is running).
#[allow(dead_code)]
fn delay_ms(ms: u32) {
    for _ in 0..ms {
        cortex_m::asm::delay(96_000);
    }
}

fn usb_vbus_detected() -> bool {
    unsafe { reg_read(GPIOA, GPIO_IDR) & (1 << 9) != 0 }
}

// ════════════════════════════════════════════════════════════════
// Buttons via resistor ladders (from pybricks platform data)
//   Ladder 0: PC4 / ADC ch14 — center button
//   Ladder 1: PA1 / ADC ch1  — left, right, BT buttons
// ════════════════════════════════════════════════════════════════

const BUTTON_CENTER_THRESHOLD: u32 = 2879;

// Resistor ladder thresholds for PA1 (left/right/BT)
const LR_LEVELS: [u32; 8] = [3872, 3394, 3009, 2755, 2538, 2327, 2141, 1969];

// Button flags
const BTN_CENTER: u8 = 0x01;
const BTN_LEFT: u8 = 0x02;
const BTN_RIGHT: u8 = 0x04;

unsafe fn init_button_adc() {
    // Enable GPIOA + GPIOC clocks
    reg_modify(RCC, RCC_AHB1ENR, 0, (1 << 0) | (1 << 2));
    // Enable ADC1 clock
    reg_modify(RCC, RCC_APB2ENR, 0, 1 << 8);
    let _ = reg_read(RCC, RCC_APB2ENR);

    // PC4 analog mode (center button)
    reg_modify(GPIOC, GPIO_MODER, 3 << 8, 3 << 8);
    // PA1 analog mode (left/right/BT buttons)
    reg_modify(GPIOA, GPIO_MODER, 3 << 2, 3 << 2);

    // ADC1: single conversion mode
    reg_write(ADC1, ADC_SQR1, 0);                   // 1 conversion
    // 480-cycle sample time for ch14 (SMPR1 bits 14:12)
    reg_modify(ADC1, ADC_SMPR1, 7 << 12, 7 << 12);
    // 480-cycle sample time for ch1 (SMPR2 bits 5:3)
    reg_modify(ADC1, ADC_SMPR2, 7 << 3, 7 << 3);

    // Power on ADC
    reg_modify(ADC1, ADC_CR2, 0, 1 << 0);
}

/// Read a single ADC channel (blocking).
fn read_adc(channel: u32) -> u32 {
    unsafe {
        reg_write(ADC1, ADC_SQR3, channel);
        reg_modify(ADC1, ADC_CR2, 0, 1 << 30); // SWSTART
        while reg_read(ADC1, ADC_SR) & (1 << 1) == 0 {}
        reg_read(ADC1, ADC_DR)
    }
}

/// Read all hub buttons, returns BTN_CENTER | BTN_LEFT | BTN_RIGHT flags.
fn read_buttons() -> u8 {
    let mut flags = 0u8;

    // Center: PC4 = ADC channel 14
    if read_adc(14) <= BUTTON_CENTER_THRESHOLD {
        flags |= BTN_CENTER;
    }

    // Left/Right/BT: PA1 = ADC channel 1
    let v = read_adc(1);
    if v <= LR_LEVELS[0] {
        if v > LR_LEVELS[1] {
            // CH_2 only = BT button (ignore)
        } else if v > LR_LEVELS[2] {
            flags |= BTN_RIGHT;                // RIGHT
        } else if v > LR_LEVELS[3] {
            flags |= BTN_RIGHT;                // RIGHT + BT
        } else if v > LR_LEVELS[4] {
            flags |= BTN_LEFT;                 // LEFT
        } else if v > LR_LEVELS[5] {
            flags |= BTN_LEFT;                 // LEFT + BT
        } else if v > LR_LEVELS[6] {
            flags |= BTN_LEFT | BTN_RIGHT;     // LEFT + RIGHT
        } else if v > LR_LEVELS[7] {
            flags |= BTN_LEFT | BTN_RIGHT;     // all
        }
    }

    flags
}

/// Coast motors and power off the hub (PA13 LOW).
fn shutdown() -> ! {
    motor_coast(Motor::A);
    motor_coast(Motor::B);
    defmt::warn!("Shutting down");
    unsafe { reg_write(GPIOA, GPIO_BSRR, 1 << (13 + 16)) };
    loop { cortex_m::asm::wfi(); }
}

// ════════════════════════════════════════════════════════════════
// LED state display
// ════════════════════════════════════════════════════════════════

fn show_paused_led() {
    const PAUSE: [u8; 25] = [
        0, 1, 0, 1, 0,
        0, 1, 0, 1, 0,
        0, 1, 0, 1, 0,
        0, 1, 0, 1, 0,
        0, 1, 0, 1, 0,
    ];
    for i in 0..25 {
        led_set_pixel(i, if PAUSE[i] != 0 { 50 } else { 0 });
    }
    unsafe { led_update() };
}

fn show_speed_led(speed: i16) {
    for i in 0..25 { led_set_pixel(i, 0); }

    let abs_speed = speed.unsigned_abs() as u32;
    let level = ((abs_speed + 100) / 200).min(5) as usize;

    // Center column: pixel indices 2, 7, 12, 17, 22 (rows 0–4)
    let col = [2usize, 7, 12, 17, 22];

    if speed > 0 {
        // Forward: bar grows UP from bottom row
        for i in 0..level { led_set_pixel(col[4 - i], 60); }
    } else if speed < 0 {
        // Reverse: bar grows DOWN from top row
        for i in 0..level { led_set_pixel(col[i], 60); }
    } else {
        // Stopped: dim center dot
        led_set_pixel(12, 20);
    }

    unsafe { led_update() };
}

// ════════════════════════════════════════════════════════════════
// LED matrix: 5×5 via TLC5955 on SPI1
// ════════════════════════════════════════════════════════════════

const TLC5955_DATA_SIZE: usize = 97;

const LED_CHANNELS: [u8; 25] = [
    38, 36, 41, 46, 33,
    37, 28, 39, 47, 21,
    24, 29, 31, 45, 23,
    26, 27, 32, 34, 22,
    25, 40, 30, 35,  9,
];

const CONTROL_LATCH: [u8; TLC5955_DATA_SIZE] = {
    let mut d = [0xFFu8; TLC5955_DATA_SIZE];
    d[0] = 0x01;
    d[1] = 0x96;
    let mut i = 2;
    while i < 50 { d[i] = 0x00; i += 1; }
    d[50] = 0x06;
    d[51] = 0x7F;
    d[52] = 0xFF;
    d[53] = 0xFE;
    d[54] = 0x00;
    d
};

static mut GRAYSCALE_BUF: [u8; TLC5955_DATA_SIZE] = [0u8; TLC5955_DATA_SIZE];

unsafe fn spi_send(data: &[u8]) {
    for &byte in data {
        while reg_read(SPI1_BASE, SPI_SR) & (1 << 1) == 0 {}
        reg_write(SPI1_BASE, SPI_DR, byte as u32);
    }
    while reg_read(SPI1_BASE, SPI_SR) & (1 << 1) == 0 {}
    while reg_read(SPI1_BASE, SPI_SR) & (1 << 7) != 0 {}
    let _ = reg_read(SPI1_BASE, SPI_DR);
    let _ = reg_read(SPI1_BASE, SPI_SR);
}

unsafe fn led_latch() {
    reg_write(GPIOA, GPIO_BSRR, 1 << 15);
    reg_write(GPIOA, GPIO_BSRR, 1 << (15 + 16));
}

fn led_set_pixel(index: usize, brightness: u16) {
    if index >= 25 { return; }
    let ch = LED_CHANNELS[index] as usize;
    let duty = (u16::MAX as u32) * (brightness as u32) * (brightness as u32) / 10000;
    let duty = duty.min(u16::MAX as u32) as u16;
    unsafe {
        GRAYSCALE_BUF[ch * 2 + 1] = (duty >> 8) as u8;
        GRAYSCALE_BUF[ch * 2 + 2] = duty as u8;
    }
}

unsafe fn led_update() {
    spi_send(core::slice::from_raw_parts(
        &raw const GRAYSCALE_BUF as *const u8,
        TLC5955_DATA_SIZE,
    ));
    led_latch();
}

unsafe fn init_led_matrix() {
    reg_modify(RCC, RCC_AHB1ENR, 0, 1 << 1);
    reg_modify(RCC, RCC_APB2ENR, 0, 1 << 12);
    reg_modify(RCC, RCC_APB1ENR, 0, 1 << 6);
    let _ = reg_read(RCC, RCC_APB1ENR);
    reg_modify(GPIOA, GPIO_MODER,
        (3 << 10) | (3 << 12) | (3 << 14),
        (2 << 10) | (2 << 12) | (2 << 14),
    );
    reg_modify(GPIOA, GPIO_OSPEEDR,
        (3 << 10) | (3 << 12) | (3 << 14),
        (3 << 10) | (3 << 12) | (3 << 14),
    );
    reg_modify(GPIOA, GPIO_AFRL,
        (0xF << 20) | (0xF << 24) | (0xF << 28),
        (5 << 20) | (5 << 24) | (5 << 28),
    );
    reg_modify(GPIOA, GPIO_MODER, 3 << 30, 1 << 30);
    reg_write(GPIOA, GPIO_BSRR, 1 << (15 + 16));
    reg_modify(GPIOB, GPIO_MODER, 3 << 30, 2 << 30);
    reg_modify(GPIOB, GPIO_AFRH, 0xF << 28, 9 << 28);
    reg_write(SPI1_BASE, SPI_CR1,
        (1 << 2) | (1 << 3) | (1 << 6) | (1 << 8) | (1 << 9),
    );
    reg_write(TIM12, TIM_PSC, 0);
    reg_write(TIM12, TIM_ARR, 9);
    reg_write(TIM12, TIM_CCR2, 4);
    reg_write(TIM12, TIM_CCMR1, (6 << 12) | (1 << 11));
    reg_write(TIM12, TIM_CCER, 1 << 4);
    reg_write(TIM12, TIM_EGR, 1);
    reg_write(TIM12, TIM_CR1, 1);
    spi_send(&CONTROL_LATCH);
    led_latch();
    spi_send(&CONTROL_LATCH);
    led_latch();
}

// ════════════════════════════════════════════════════════════════
// RTIC application
// ════════════════════════════════════════════════════════════════

#[rtic::app(device = stm32f4::stm32f413, dispatchers = [USART2])]
mod app {
    use super::*;
    use rtic_monotonics::systick::prelude::*;

    systick_monotonic!(Mono, 1000);

    #[shared]
    struct Shared {}

    #[local]
    struct Local {}

    #[init]
    fn init(cx: init::Context) -> (Shared, Local) {
        defmt::info!("Hub booting...");

        unsafe {
            // PA13 power hold — must be immediate
            reg_modify(RCC, RCC_AHB1ENR, 0, 1 << 0);
            let _ = reg_read(RCC, RCC_AHB1ENR);
            reg_modify(GPIOA, GPIO_MODER, 3 << 26, 1 << 26);
            reg_write(GPIOA, GPIO_BSRR, 1 << 13);

            init_clocks();
            init_power_and_gpio();
            init_timer();
            init_led_matrix();
            init_button_adc();
        }

        defmt::info!("Hardware initialized");

        // Heart pattern on LED matrix
        const HEART: [u8; 25] = [
            0, 1, 0, 1, 0,
            1, 1, 1, 1, 1,
            1, 1, 1, 1, 1,
            0, 1, 1, 1, 0,
            0, 0, 1, 0, 0,
        ];
        for i in 0..25 {
            led_set_pixel(i, if HEART[i] != 0 { 50 } else { 0 });
        }
        unsafe { led_update() };
        defmt::info!("LED matrix: heart");

        // Start SysTick monotonic (96 MHz HCLK)
        Mono::start(cx.core.SYST, 96_000_000);

        motor_demo::spawn().ok();
        defmt::info!("Tasks spawned");

        (Shared {}, Local {})
    }

    #[task(priority = 1)]
    async fn motor_demo(_cx: motor_demo::Context) {
        // Startup kick
        motor_coast(Motor::A);
        motor_coast(Motor::B);
        Mono::delay(500.millis()).await;

        motor_set(Motor::A, 400);
        motor_set(Motor::B, 400);
        Mono::delay(150.millis()).await;
        motor_coast(Motor::A);
        motor_coast(Motor::B);
        Mono::delay(500.millis()).await;

        motor_set(Motor::A, -400);
        motor_set(Motor::B, -400);
        Mono::delay(150.millis()).await;
        motor_coast(Motor::A);
        motor_coast(Motor::B);
        Mono::delay(1000.millis()).await;

        // Dev mode detection
        unsafe { reg_modify(GPIOA, GPIO_MODER, 3 << 18, 0) };
        let dev_mode = usb_vbus_detected();
        defmt::info!("dev_mode = {}", dev_mode);

        // ── Dev mode: quick demo then re-enter DFU for fast iteration ──
        if dev_mode {
            // Brief forward burst to confirm firmware is alive
            motor_set(Motor::A, 500);
            motor_set(Motor::B, 500);
            Mono::delay(400.millis()).await;
            motor_coast(Motor::A);
            motor_coast(Motor::B);
            Mono::delay(300.millis()).await;

            // Brief reverse burst
            motor_set(Motor::A, -500);
            motor_set(Motor::B, -500);
            Mono::delay(400.millis()).await;
            motor_coast(Motor::A);
            motor_coast(Motor::B);

            // Show checkmark pattern to confirm dev cycle OK
            const CHECK: [u8; 25] = [
                0, 0, 0, 0, 1,
                0, 0, 0, 1, 0,
                0, 0, 1, 0, 0,
                1, 0, 1, 0, 0,
                0, 1, 0, 0, 0,
            ];
            for i in 0..25 {
                led_set_pixel(i, if CHECK[i] != 0 { 80 } else { 0 });
            }
            unsafe { led_update() };
            defmt::info!("Dev cycle done — entering DFU");
            Mono::delay(300.millis()).await;

            request_dfu();
        }

        // ── Interactive standalone mode ──
        // Center: short press = pause/resume, long press (2s) = shutdown
        // Left:  decrease speed (step 200)
        // Right: increase speed (step 200)
        // LED:   speed bar (center column) or pause icon

        let mut paused = false;
        let mut speed: i16 = 400;
        let mut prev_btns = read_buttons(); // seed to avoid spurious edges
        let mut center_hold: u32 = 0;

        defmt::info!("Interactive mode: center=pause, L/R=speed, long-center=off");

        show_speed_led(speed);
        motor_set(Motor::A, speed);
        motor_set(Motor::B, speed);

        loop {
            let btns = read_buttons();

            // ── Center button: long press = shutdown ──
            if btns & BTN_CENTER != 0 {
                center_hold += 1;
                if center_hold >= 40 { // 40 × 50 ms = 2 s
                    shutdown();
                }
            } else {
                // Released after short press → toggle pause
                if prev_btns & BTN_CENTER != 0 && center_hold > 0 && center_hold < 40 {
                    paused = !paused;
                    if paused {
                        motor_coast(Motor::A);
                        motor_coast(Motor::B);
                        show_paused_led();
                        defmt::info!("PAUSED");
                    } else {
                        motor_set(Motor::A, speed);
                        motor_set(Motor::B, speed);
                        show_speed_led(speed);
                        defmt::info!("RUNNING speed={}", speed);
                    }
                }
                center_hold = 0;
            }

            // ── Left / Right: speed control (only while running) ──
            let rising = btns & !prev_btns;

            if !paused {
                if rising & BTN_LEFT != 0 {
                    speed = (speed - 200).max(-1000);
                    motor_set(Motor::A, speed);
                    motor_set(Motor::B, speed);
                    show_speed_led(speed);
                    defmt::info!("speed={}", speed);
                }
                if rising & BTN_RIGHT != 0 {
                    speed = (speed + 200).min(1000);
                    motor_set(Motor::A, speed);
                    motor_set(Motor::B, speed);
                    show_speed_led(speed);
                    defmt::info!("speed={}", speed);
                }
            }

            prev_btns = btns;
            Mono::delay(50.millis()).await;
        }
    }
}
