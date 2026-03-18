//! LED test binary for LEGO SPIKE Prime Hub (STM32F413VGT6)
//!
//! Experiments with the TLC5955 LED driver:
//!   - Status LED top/bottom (center button ring)
//!   - Battery LED
//!   - Bluetooth LED
//!   - 5×5 Light Matrix
//!   - Ring-button behavior test (SPIKE-like):
//!       * long center press -> power down (standby)
//!       * left+right near-simultaneous press -> toggle resident program
//!         (LED show + small motor ticks)
//!
//! Launched via monitor's 'run' command (0x08010000).
//! Properly initializes TLC5955 control register before sending
//! greyscale data — the step the monitor was missing.

#![no_std]
#![no_main]

use core::ptr;

// ════════════════════════════════════════════════════════════════
// DebugMonitor trampoline — forwards to monitor's handler
// ════════════════════════════════════════════════════════════════

core::arch::global_asm!(
    ".section .text",
    ".global DebugMonitor",
    ".type DebugMonitor, %function",
    ".thumb_func",
    "DebugMonitor:",
    "ldr r12, =0x2004FFE0",
    "ldr r12, [r12]",
    "cmp r12, #0",
    "beq 1f",
    "bx r12",
    "1:",
    "bx lr",
);

// ════════════════════════════════════════════════════════════════
// SysTick trampoline — forwards to monitor's SysTick handler
// ════════════════════════════════════════════════════════════════

core::arch::global_asm!(
    ".section .text",
    ".global SysTick",
    ".type SysTick, %function",
    ".thumb_func",
    "SysTick:",
    "ldr r12, =0x2004FFE4",
    "ldr r12, [r12]",
    "cmp r12, #0",
    "beq 1f",
    "bx r12",
    "1:",
    "bx lr",
);

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    loop { cortex_m::asm::wfi(); }
}

// ════════════════════════════════════════════════════════════════
// Register bases
// ════════════════════════════════════════════════════════════════

const RCC: u32 = 0x4002_3800;
const FLASH_R: u32 = 0x4002_3C00;
const GPIOA: u32 = 0x4002_0000;
const GPIOB: u32 = 0x4002_0400;
const GPIOC: u32 = 0x4002_0800;
const GPIOE: u32 = 0x4002_1000;
const SPI1: u32 = 0x4001_3000;
const ADC1: u32 = 0x4001_2000;
const TIM1: u32 = 0x4001_0000;
const TIM12: u32 = 0x4000_1800;

// ════════════════════════════════════════════════════════════════
// TLC5955 channel map (pybricks numbering)
// ════════════════════════════════════════════════════════════════

// Status LED top:    R=5,  G=4,  B=3
// Status LED bottom: R=8,  G=7,  B=6
// Battery LED:       R=2,  G=1,  B=0
// Bluetooth LED:     R=20, G=19, B=18

// Light matrix 5×5 (row-major, top-left origin):
const MATRIX: [u8; 25] = [
    38, 36, 41, 46, 33,
    37, 28, 39, 47, 21,
    24, 29, 31, 45, 23,
    26, 27, 32, 34, 22,
    25, 40, 30, 35,  9,
];

// Button thresholds and flags (from prime_hub resistor ladder)
const BUTTON_CENTER_THRESHOLD: u32 = 2879;
const LR_LEVELS: [u32; 8] = [3872, 3394, 3009, 2755, 2538, 2327, 2141, 1969];

const BTN_CENTER: u8 = 0x01;
const BTN_LEFT: u8 = 0x02;
const BTN_RIGHT: u8 = 0x04;

// Interaction timing (20 ms control loop)
const LOOP_MS: u32 = 20;
const CENTER_LONG_PRESS_TICKS: u32 = 100; // 2.0 s
const SIDE_SYNC_WINDOW_TICKS: u32 = 8; // 160 ms tolerance
const MOTOR_MIN_PERIOD: u32 = 40; // 800 ms min between moves
const MOTOR_PERIOD_RANGE: u32 = 60; // +0..1200 ms random
const MOTOR_MIN_ON: u32 = 3; // 60 ms min pulse
const MOTOR_ON_RANGE: u32 = 5; // +0..100 ms random (max ~160 ms ≈ 90°)
const MOTOR_DUTY: i16 = 500;

// ════════════════════════════════════════════════════════════════
// Register access helpers
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

fn delay_ms(ms: u32) {
    cortex_m::asm::delay(ms * 96_000); // 96 MHz
}

// ════════════════════════════════════════════════════════════════
// Buttons (ADC resistor ladder)
// ════════════════════════════════════════════════════════════════

unsafe fn init_button_adc() {
    // GPIOA + GPIOC + ADC1 clocks
    reg_modify(RCC, 0x30, 0, (1 << 0) | (1 << 2));
    reg_modify(RCC, 0x44, 0, 1 << 8);
    let _ = reg_read(RCC, 0x44);

    // PC4 analog (center), PA1 analog (left/right/bt ladder)
    reg_modify(GPIOC, 0x00, 3 << 8, 3 << 8);
    reg_modify(GPIOA, 0x00, 3 << 2, 3 << 2);

    // ADC1 single conversion, long sample time for stable ladder reads
    reg_write(ADC1, 0x2C, 0); // SQR1: 1 conversion
    reg_modify(ADC1, 0x0C, 7 << 12, 7 << 12); // SMPR1 ch14
    reg_modify(ADC1, 0x10, 7 << 3, 7 << 3); // SMPR2 ch1
    reg_modify(ADC1, 0x08, 0, 1 << 0); // CR2 ADON
}

fn read_adc(channel: u32) -> u32 {
    unsafe {
        // Disable interrupts to prevent the monitor's SysTick handler from
        // reconfiguring/disabling ADC1 mid-conversion (it turns off ADON
        // after each button poll).
        cortex_m::interrupt::disable();
        reg_write(ADC1, 0x34, channel); // SQR3
        reg_modify(ADC1, 0x08, 0, 1 << 0); // ensure ADON
        reg_write(ADC1, 0x00, 0); // clear SR
        reg_modify(ADC1, 0x08, 0, 1 << 30); // SWSTART
        // Wait for EOC with timeout — never spin forever
        let mut timeout = 100_000u32;
        while reg_read(ADC1, 0x00) & (1 << 1) == 0 {
            timeout -= 1;
            if timeout == 0 {
                // ADC stuck — re-init ADON and return max (no press)
                reg_modify(ADC1, 0x08, 1 << 0, 0); // clear ADON
                cortex_m::asm::delay(100);
                reg_modify(ADC1, 0x08, 0, 1 << 0); // set ADON
                cortex_m::interrupt::enable();
                return 4095;
            }
        }
        let val = reg_read(ADC1, 0x4C); // DR
        cortex_m::interrupt::enable();
        val
    }
}

fn read_buttons() -> u8 {
    let mut flags = 0u8;

    // Center button on PC4 / ADC14
    if read_adc(14) <= BUTTON_CENTER_THRESHOLD {
        flags |= BTN_CENTER;
    }

    // Left/Right on PA1 / ADC1 (via resistor ladder bins)
    let v = read_adc(1);
    if v <= LR_LEVELS[0] {
        if v > LR_LEVELS[1] {
            // BT-only, ignore
        } else if v > LR_LEVELS[2] {
            flags |= BTN_RIGHT;
        } else if v > LR_LEVELS[3] {
            flags |= BTN_RIGHT; // right+bt
        } else if v > LR_LEVELS[4] {
            flags |= BTN_LEFT;
        } else if v > LR_LEVELS[5] {
            flags |= BTN_LEFT; // left+bt
        } else if v > LR_LEVELS[6] {
            flags |= BTN_LEFT | BTN_RIGHT;
        } else if v > LR_LEVELS[7] {
            flags |= BTN_LEFT | BTN_RIGHT; // all
        }
    }

    flags
}

// ════════════════════════════════════════════════════════════════
// PA13 power hold — before .bss/.data init
// ════════════════════════════════════════════════════════════════

#[cortex_m_rt::pre_init]
unsafe fn pre_init() {
    let rcc_ahb1enr = (RCC + 0x30) as *mut u32;
    ptr::write_volatile(rcc_ahb1enr, ptr::read_volatile(rcc_ahb1enr) | 1);
    let _ = ptr::read_volatile(rcc_ahb1enr);
    let moder = GPIOA as *mut u32;
    let v = ptr::read_volatile(moder);
    ptr::write_volatile(moder, (v & !(3 << 26)) | (1 << 26));
    ptr::write_volatile((GPIOA + 0x18) as *mut u32, 1 << 13);
}

// ════════════════════════════════════════════════════════════════
// Clock init (96 MHz PLL from 16 MHz HSE)
// Skip if PLL already running (launched from monitor)
// ════════════════════════════════════════════════════════════════

unsafe fn init_clocks() {
    // Already on PLL? Skip.
    if (reg_read(RCC, 0x08) & 0xC) == 0x8 {
        return;
    }
    reg_modify(FLASH_R, 0x00, 0xF, 3);
    reg_modify(RCC, 0x00, 0, 1 << 16);
    while reg_read(RCC, 0x00) & (1 << 17) == 0 {}
    reg_write(RCC, 0x04, 16 | (192 << 6) | (0 << 16) | (1 << 22) | (4 << 24));
    reg_modify(RCC, 0x00, 0, 1 << 24);
    while reg_read(RCC, 0x00) & (1 << 25) == 0 {}
    reg_modify(RCC, 0x08, 0xFCF0, 0x1000);
    reg_modify(RCC, 0x08, 0x3, 0x2);
    while (reg_read(RCC, 0x08) & 0xC) != 0x8 {}
}

// ════════════════════════════════════════════════════════════════
// Speaker beep (startup indicator)
// ════════════════════════════════════════════════════════════════

unsafe fn beep(duration_ms: u32, freq_hz: u32) {
    reg_modify(RCC, 0x30, 0, (1 << 0) | (1 << 2));
    let _ = reg_read(RCC, 0x30);
    reg_modify(GPIOC, 0x00, 3 << 20, 1 << 20); // PC10 output
    reg_write(GPIOC, 0x18, 1 << 10);            // amp enable
    reg_modify(GPIOA, 0x00, 3 << 8, 1 << 8);    // PA4 output
    let half = 96_000_000 / (freq_hz * 2);
    let toggles = duration_ms * freq_hz * 2 / 1000;
    for _ in 0..toggles {
        reg_write(GPIOA, 0x18, 1 << 4);
        cortex_m::asm::delay(half);
        reg_write(GPIOA, 0x18, 1 << (4 + 16));
        cortex_m::asm::delay(half);
    }
    reg_write(GPIOC, 0x18, 1 << (10 + 16)); // amp off
    reg_modify(GPIOA, 0x00, 3 << 8, 3 << 8); // PA4 analog
}

// ════════════════════════════════════════════════════════════════
// Small motor tick support (ports A+B H-bridge via TIM1)
// ════════════════════════════════════════════════════════════════

unsafe fn init_motor_hw() {
    // Enable GPIOA + GPIOE + TIM1 clocks
    reg_modify(RCC, 0x30, 0, (1 << 0) | (1 << 4));
    reg_modify(RCC, 0x44, 0, 1 << 0);
    let _ = reg_read(RCC, 0x44);

    // PA14 output HIGH: enable I/O port VCC
    reg_modify(GPIOA, 0x00, 3 << 28, 1 << 28);
    reg_write(GPIOA, 0x18, 1 << 14);

    // PE9/11/13/14 AF1 (TIM1 CH1..CH4)
    let af_clear = (0xF << 4) | (0xF << 12) | (0xF << 20) | (0xF << 24);
    let af_set = (1 << 4) | (1 << 12) | (1 << 20) | (1 << 24);
    reg_modify(GPIOE, 0x24, af_clear, af_set);

    // High speed on these pins
    let speed_mask = (3 << 18) | (3 << 22) | (3 << 26) | (3 << 28);
    reg_modify(GPIOE, 0x08, speed_mask, speed_mask);

    // Start as GPIO outputs (coast)
    let moder_mask = (3u32 << 18) | (3 << 22) | (3 << 26) | (3 << 28);
    let moder_out = (1u32 << 18) | (1 << 22) | (1 << 26) | (1 << 28);
    reg_modify(GPIOE, 0x00, moder_mask, moder_out);
    reg_write(GPIOE, 0x18, (1 << (9 + 16)) | (1 << (11 + 16)) | (1 << (13 + 16)) | (1 << (14 + 16)));

    // TIM1 PWM: 12 kHz, ARR=999, PSC=7
    reg_write(TIM1, 0x28, 7); // PSC
    reg_write(TIM1, 0x2C, 999); // ARR
    reg_write(TIM1, 0x18, 0x6868); // CCMR1 CH1/2 PWM1 + preload
    reg_write(TIM1, 0x1C, 0x6868); // CCMR2 CH3/4 PWM1 + preload
    reg_write(TIM1, 0x20, 0x3333); // CCER polarity inverted (matches hub wiring)
    reg_write(TIM1, 0x34, 0); // CCR1
    reg_write(TIM1, 0x38, 0); // CCR2
    reg_write(TIM1, 0x3C, 0); // CCR3
    reg_write(TIM1, 0x40, 0); // CCR4
    reg_write(TIM1, 0x44, 1 << 15); // BDTR MOE
    reg_write(TIM1, 0x00, (1 << 7) | (1 << 0)); // CR1 ARPE + CEN
    reg_write(TIM1, 0x14, 1); // EGR UG
}

unsafe fn set_motor_a(duty: i16) {
    let duty = duty.clamp(-1000, 1000);
    if duty > 0 {
        reg_write(TIM1, 0x34, duty as u32); // CCR1
        reg_modify(GPIOE, 0x00, (3 << 18) | (3 << 22), (2 << 18) | (1 << 22));
        reg_write(GPIOE, 0x18, 1 << 11);
    } else if duty < 0 {
        reg_write(TIM1, 0x38, (-duty) as u32); // CCR2
        reg_modify(GPIOE, 0x00, (3 << 18) | (3 << 22), (1 << 18) | (2 << 22));
        reg_write(GPIOE, 0x18, 1 << 9);
    } else {
        reg_modify(GPIOE, 0x00, (3 << 18) | (3 << 22), (1 << 18) | (1 << 22));
        reg_write(GPIOE, 0x18, (1 << (9 + 16)) | (1 << (11 + 16)));
    }
}

unsafe fn set_motor_b(duty: i16) {
    let duty = duty.clamp(-1000, 1000);
    if duty > 0 {
        reg_write(TIM1, 0x3C, duty as u32); // CCR3
        reg_modify(GPIOE, 0x00, (3 << 26) | (3 << 28), (2 << 26) | (1 << 28));
        reg_write(GPIOE, 0x18, 1 << 14);
    } else if duty < 0 {
        reg_write(TIM1, 0x40, (-duty) as u32); // CCR4
        reg_modify(GPIOE, 0x00, (3 << 26) | (3 << 28), (1 << 26) | (2 << 28));
        reg_write(GPIOE, 0x18, 1 << 13);
    } else {
        reg_modify(GPIOE, 0x00, (3 << 26) | (3 << 28), (1 << 26) | (1 << 28));
        reg_write(GPIOE, 0x18, (1 << (13 + 16)) | (1 << (14 + 16)));
    }
}

#[allow(dead_code)]
unsafe fn motor_set_both(duty: i16) {
    set_motor_a(duty);
    set_motor_b(duty);
}

unsafe fn motor_coast_all() {
    set_motor_a(0);
    set_motor_b(0);
}

fn shutdown_hub() -> ! {
    unsafe {
        motor_coast_all();
        // PA13 LOW => cut power hold (deep-sleep/standby by PMIC path)
        reg_write(GPIOA, 0x18, 1 << (13 + 16));
    }
    loop { cortex_m::asm::wfi(); }
}

// Simple xorshift32 PRNG (no_std friendly)
static mut RNG_STATE: u32 = 0xDEAD_BEEF;

fn rng_next() -> u32 {
    unsafe {
        let mut s = RNG_STATE;
        s ^= s << 13;
        s ^= s >> 17;
        s ^= s << 5;
        RNG_STATE = s;
        s
    }
}

/// Seed the PRNG with something entropy-ish (SysTick counter + ADC noise)
fn rng_seed() {
    unsafe {
        let systick_val = ptr::read_volatile(0xE000_E018 as *const u32);
        let adc_noise = read_adc(14) ^ read_adc(1);
        RNG_STATE = systick_val ^ adc_noise ^ 0xCAFE_BABE;
        // Warm up
        for _ in 0..8 { rng_next(); }
    }
}

// ════════════════════════════════════════════════════════════════
// TLC5955 driver
// ════════════════════════════════════════════════════════════════

/// Initialize SPI1, TIM12 GSCLK, and GPIO for TLC5955
unsafe fn tlc5955_init_hw() {
    // Enable clocks: GPIOA, GPIOB, SPI1, TIM12
    reg_modify(RCC, 0x30, 0, (1 << 0) | (1 << 1));
    reg_modify(RCC, 0x44, 0, 1 << 12);  // SPI1 (APB2)
    reg_modify(RCC, 0x40, 0, 1 << 6);   // TIM12 (APB1)
    let _ = reg_read(RCC, 0x30);

    // Disable SPI first (in case monitor left it on)
    reg_write(SPI1, 0x00, 0);

    // PA5 = AF5 (SPI1_SCK)
    reg_modify(GPIOA, 0x20, 0xF << 20, 5 << 20);
    reg_modify(GPIOA, 0x00, 3 << 10, 2 << 10);
    reg_modify(GPIOA, 0x08, 3 << 10, 3 << 10);

    // PA7 = AF5 (SPI1_MOSI)
    reg_modify(GPIOA, 0x20, 0xF << 28, 5 << 28);
    reg_modify(GPIOA, 0x00, 3 << 14, 2 << 14);
    reg_modify(GPIOA, 0x08, 3 << 14, 3 << 14);

    // PA15 = GPIO output (LAT) — clear JTDI AF first
    reg_modify(GPIOA, 0x24, 0xF << 28, 0);
    reg_modify(GPIOA, 0x00, 3 << 30, 1 << 30);
    reg_write(GPIOA, 0x18, 1 << (15 + 16)); // LAT LOW

    // PB15 = AF9 (TIM12_CH2 = GSCLK)
    reg_modify(GPIOB, 0x24, 0xF << 28, 9 << 28);
    reg_modify(GPIOB, 0x00, 3 << 30, 2 << 30);
    reg_modify(GPIOB, 0x08, 3 << 30, 3 << 30);

    // TIM12 CH2: ~9.6 MHz PWM (GSCLK)
    reg_write(TIM12, 0x00, 0);    // disable first
    reg_write(TIM12, 0x28, 0);    // PSC = 0
    reg_write(TIM12, 0x2C, 4);    // ARR = 4 (48MHz/5 = 9.6MHz)
    // OC2M = PWM mode 1 (110), OC2PE = 1
    reg_write(TIM12, 0x18, (6 << 12) | (1 << 11));
    reg_write(TIM12, 0x38, 2);    // CCR2 = 2 (50% duty)
    reg_write(TIM12, 0x20, 1 << 4); // CC2E
    reg_write(TIM12, 0x00, 1);    // CEN

    // SPI1: master, 8-bit, SW NSS, BR=011 (/16 → 6 MHz)
    reg_write(SPI1, 0x00, (1 << 2) | (3 << 3) | (1 << 9) | (1 << 8));
    reg_modify(SPI1, 0x00, 0, 1 << 6); // SPE
}

/// Send raw bytes over SPI1 and wait for completion
unsafe fn spi_send(data: &[u8]) {
    for &byte in data {
        while reg_read(SPI1, 0x08) & (1 << 1) == 0 {} // TXE
        reg_write(SPI1, 0x0C, byte as u32);
    }
    while reg_read(SPI1, 0x08) & (1 << 7) != 0 {} // BSY
}

/// Pulse LAT to latch data into TLC5955
unsafe fn tlc5955_latch() {
    reg_write(GPIOA, 0x18, 1 << 15);        // LAT HIGH
    cortex_m::asm::delay(100);                // >30 ns
    reg_write(GPIOA, 0x18, 1 << (15 + 16)); // LAT LOW
}

/// Send TLC5955 control register.
/// Must be called TWICE for max-current settings to take effect.
/// Parameters: dc=127, mc=0 (3.2mA), bc=127, dsprpt=1, espwm=1, lsdvlt=1
unsafe fn tlc5955_send_control() {
    let mut ctrl = [0u8; 97];
    ctrl[0] = 1;      // bit 768 = 1 → control mode
    ctrl[1] = 0x96;   // constant identifier

    // bytes 2..49 = 0 (reserved)

    // Byte 50: LSDVLT=1, ESPWM=1, RFRESH=0
    ctrl[50] = (1 << 2) | (1 << 1); // = 6

    // Byte 51: TMGRST=0, DSPRPT=1, BC_B[6:1]
    ctrl[51] = (1 << 6) | (127 >> 1); // = 0x7F

    // Byte 52: BC_B[0] | BC_G[6:0]
    ctrl[52] = (127 << 7) as u8 | 127; // = 0xFF

    // Byte 53: BC_R[6:0] | MC_B[2] (mc=0 → bit=0)
    ctrl[53] = (127 << 1) as u8; // = 0xFE

    // Byte 54: MC_B[1:0] | MC_G[2:0] | MC_R[2:0] (all 0)
    ctrl[54] = 0;

    // Bytes 55..96: DC values (48 channels × 7 bits = 336 bits = 42 bytes)
    // dc=127 (all 1s) → every byte is 0xFF
    for b in ctrl[55..97].iter_mut() {
        *b = 0xFF;
    }

    spi_send(&ctrl);
    tlc5955_latch();
}

/// Set all 48 TLC5955 channels from a brightness array (0..65535).
/// Channel mapping follows pybricks convention.
unsafe fn tlc5955_send_gs(channels: &[u16; 48]) {
    let mut frame = [0u8; 97];
    frame[0] = 0; // bit 768 = 0 → greyscale mode
    for ch in 0..48 {
        frame[ch * 2 + 1] = (channels[ch] >> 8) as u8;
        frame[ch * 2 + 2] = channels[ch] as u8;
    }
    spi_send(&frame);
    tlc5955_latch();
}

unsafe fn show_idle_frame() {
    let mut gs = [0u16; 48];

    // Dim center matrix pixel
    gs[MATRIX[12] as usize] = 0x1000;

    // Status top ring: soft blue while idle
    gs[3] = 0x0800;

    tlc5955_send_gs(&gs);
}

unsafe fn show_resident_frame(frame: u32) {
    let mut gs = [0u16; 48];

    // Status ring breathing green
    let p = (frame % 20) as i32;
    let tri = if p < 10 { p } else { 19 - p } as u16;
    let breath = 0x0800 + (tri * 0x0200);
    gs[4] = breath;

    // Moving dot on matrix + faint tail
    let idx = (frame as usize) % 25;
    gs[MATRIX[idx] as usize] = 0x3000;
    gs[MATRIX[(idx + 24) % 25] as usize] = 0x1000;

    // Keep BT LED dim cyan-ish as "resident running" marker
    gs[19] = 0x0800;
    gs[18] = 0x0800;

    tlc5955_send_gs(&gs);
}

unsafe fn toggle_resident_program(running: &mut bool, frame: &mut u32) {
    *running = !*running;
    *frame = 0;
    if *running {
        show_resident_frame(0);
        motor_coast_all();
    } else {
        motor_coast_all();
        show_idle_frame();
    }
}

// ════════════════════════════════════════════════════════════════
// Entry
// ════════════════════════════════════════════════════════════════

#[cortex_m_rt::entry]
fn main() -> ! {
    unsafe {
        init_clocks();

        // Two short beeps = "LED test app starting"
        beep(80, 1500);
        delay_ms(80);
        beep(80, 2000);

        // Initialize TLC5955 hardware
        tlc5955_init_hw();

        // CRITICAL: send control register TWICE (TLC5955 requirement)
        tlc5955_send_control();
        tlc5955_send_control();

        // Initialize button ADC and motor hardware for ring-button feature tests
        init_button_adc();
        init_motor_hw();

        show_idle_frame();

        let mut prev_btns = read_buttons();
        let mut center_hold_ticks = 0u32;

        let mut resident_running = false;
        let mut resident_frame = 0u32;

        // Motor random rotation state
        let mut motor_countdown: u32 = 0; // frames until next move
        let mut motor_on_left: u32 = 0; // frames motor A still on
        let mut _motor_on_left_duty: i16 = 0;
        let mut motor_on_right: u32 = 0; // frames motor B still on
        let mut _motor_on_right_duty: i16 = 0;

        rng_seed();

        // Simultaneous-press tolerance state
        let mut pending_side: u8 = 0;
        let mut pending_ticks: u32 = 0;
        let mut combo_latch = false;

        loop {
            let btns = read_buttons();
            let rising = btns & !prev_btns;

            // Short click on ANY button press — audible feedback for dev/user
            if rising != 0 {
                beep(10, 4000);
            }

            // Long center-ring press => power down
            if btns & BTN_CENTER != 0 {
                center_hold_ticks += 1;
                if center_hold_ticks >= CENTER_LONG_PRESS_TICKS {
                    shutdown_hub();
                }
            } else {
                center_hold_ticks = 0;
            }

            // Left+Right near-simultaneous => toggle resident program
            if !combo_latch {
                let side_rising = rising & (BTN_LEFT | BTN_RIGHT);

                if side_rising == (BTN_LEFT | BTN_RIGHT) {
                    toggle_resident_program(&mut resident_running, &mut resident_frame);
                    combo_latch = true;
                    pending_side = 0;
                    pending_ticks = 0;
                } else {
                    if side_rising == BTN_LEFT || side_rising == BTN_RIGHT {
                        pending_side = side_rising;
                        pending_ticks = SIDE_SYNC_WINDOW_TICKS;
                    }

                    if pending_side != 0 {
                        let other = if pending_side == BTN_LEFT { BTN_RIGHT } else { BTN_LEFT };
                        let pending_still_down = (btns & pending_side) != 0;
                        let other_now_down = (btns & other) != 0;

                        if pending_still_down && other_now_down {
                            toggle_resident_program(&mut resident_running, &mut resident_frame);
                            combo_latch = true;
                            pending_side = 0;
                            pending_ticks = 0;
                        } else {
                            if pending_ticks > 0 {
                                pending_ticks -= 1;
                            }
                            if pending_ticks == 0 || !pending_still_down {
                                pending_side = 0;
                            }
                        }
                    }
                }
            }

            // Rearm combo detector once both side buttons are no longer held together
            if btns & (BTN_LEFT | BTN_RIGHT) != (BTN_LEFT | BTN_RIGHT) {
                combo_latch = false;
            }

            // Resident behavior: LED show + random motor rotations
            if resident_running {
                show_resident_frame(resident_frame);

                // Independent random rotation for each motor
                if motor_countdown == 0 {
                    // Pick random duration and direction for each motor independently
                    let r = rng_next();
                    let on_a = MOTOR_MIN_ON + (r % (MOTOR_ON_RANGE + 1));
                    let dir_a = if r & 0x100 != 0 { MOTOR_DUTY } else { -MOTOR_DUTY };

                    let r2 = rng_next();
                    let on_b = MOTOR_MIN_ON + (r2 % (MOTOR_ON_RANGE + 1));
                    let dir_b = if r2 & 0x100 != 0 { MOTOR_DUTY } else { -MOTOR_DUTY };

                    motor_on_left = on_a;
                    _motor_on_left_duty = dir_a;
                    motor_on_right = on_b;
                    _motor_on_right_duty = dir_b;

                    set_motor_a(dir_a);
                    set_motor_b(dir_b);

                    let r3 = rng_next();
                    motor_countdown = MOTOR_MIN_PERIOD + (r3 % (MOTOR_PERIOD_RANGE + 1));
                } else {
                    motor_countdown -= 1;
                }

                // Stop each motor after its on-time expires
                if motor_on_left > 0 {
                    motor_on_left -= 1;
                    if motor_on_left == 0 { set_motor_a(0); }
                }
                if motor_on_right > 0 {
                    motor_on_right -= 1;
                    if motor_on_right == 0 { set_motor_b(0); }
                }

                resident_frame = resident_frame.wrapping_add(1);
            }

            prev_btns = btns;
            delay_ms(LOOP_MS);
        }
    }
}
