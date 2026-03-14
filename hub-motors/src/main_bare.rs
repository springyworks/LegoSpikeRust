//! Bare-metal motor control for LEGO SPIKE Prime / Robot Inventor 51515 hub
//!
//! Controls two motors on **Port A** and **Port B** using direct register access.
//! No HAL, no PAC, no RTOS — just Rust talking to STM32F413 registers.
//!
//! Hardware (from pybricks platform.c):
//!   MCU: STM32F413VGT6 — Cortex-M4F, 1 MB Flash, 320 KB RAM
//!   HSE: 16 MHz crystal → PLL → 96 MHz SYSCLK
//!
//!   Port A motor H-bridge: PE9  (TIM1_CH1, AF1) + PE11 (TIM1_CH2, AF1)
//!   Port B motor H-bridge: PE13 (TIM1_CH3, AF1) + PE14 (TIM1_CH4, AF1)
//!   PA13: power hold (LOW = power off!)
//!   PA14: I/O port VCC enable
//!
//!   PWM: 12 kHz (prescaler=8, period=1000), inverted polarity
//!   H-bridge: fwd = pin1 PWM + pin2 HIGH
//!             rev = pin1 HIGH + pin2 PWM
//!             brake = both HIGH, coast = both LOW

#![no_std]
#![no_main]

use core::ptr;
use cortex_m_rt::entry;
use panic_halt as _;

// ════════════════════════════════════════════════════════════════
// Register base addresses (STM32F413 reference manual)
// ════════════════════════════════════════════════════════════════
const RCC: u32 = 0x4002_3800;
const FLASH_R: u32 = 0x4002_3C00;
const GPIOA: u32 = 0x4002_0000;
const GPIOB: u32 = 0x4002_0400;
const GPIOE: u32 = 0x4002_1000;
const SPI1: u32 = 0x4001_3000;
const TIM1: u32 = 0x4001_0000;
const TIM12: u32 = 0x4000_1800;
const SYSCFG: u32 = 0x4001_3800;

// RCC register offsets
const RCC_CR: u32 = 0x00;
const RCC_PLLCFGR: u32 = 0x04;
const RCC_CFGR: u32 = 0x08;
const RCC_AHB1ENR: u32 = 0x30;
const RCC_APB1ENR: u32 = 0x40;
const RCC_APB2ENR: u32 = 0x44;
const RCC_CSR: u32 = 0x74;

// FLASH
const FLASH_ACR: u32 = 0x00;

// GPIO register offsets
const GPIO_MODER: u32 = 0x00;
const GPIO_IDR: u32 = 0x10;
const GPIO_OSPEEDR: u32 = 0x08;
const GPIO_BSRR: u32 = 0x18;
const GPIO_AFRL: u32 = 0x20;
const GPIO_AFRH: u32 = 0x24;

// TIM1 register offsets (advanced-control timer)
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

// ════════════════════════════════════════════════════════════════
// DFU re-entry: jump to STM32 system bootloader (no manual button!)
//
// On boot, if a magic value is in RAM → jump to 0x1FFF0000 (ST DFU).
// From running code: write magic to RAM + NVIC_SystemReset().
// RAM survives software reset, so next boot sees the flag.
//
// dev.sh flow: detect hub on USB → send USB reset → hub reboots →
// sees magic → enters ST DFU → dev.sh flashes → `:leave` → reboot.
// ════════════════════════════════════════════════════════════════

/// Magic address near end of 320KB RAM (0x20000000 + 320K - 16)
const DFU_MAGIC_ADDR: *mut u32 = 0x2004_FFF0 as *mut u32;
const DFU_MAGIC_VALUE: u32 = 0xDEAD_B007;

/// Write magic value and reset — next boot enters system DFU.
#[allow(dead_code)]
pub fn request_dfu() -> ! {
    unsafe {
        ptr::write_volatile(DFU_MAGIC_ADDR, DFU_MAGIC_VALUE);
    }
    cortex_m::peripheral::SCB::sys_reset();
}

/// Check for magic value; if set, jump to STM32 system bootloader.
/// Must be called very early, before PA13 power hold is set up.
unsafe fn check_and_enter_dfu() {
    if ptr::read_volatile(DFU_MAGIC_ADDR) != DFU_MAGIC_VALUE {
        return;
    }
    // Clear flag so we don't loop
    ptr::write_volatile(DFU_MAGIC_ADDR, 0);

    // Disable all interrupts
    cortex_m::interrupt::disable();

    // Disable SysTick
    ptr::write_volatile(0xE000_E010 as *mut u32, 0);

    // Clear pending interrupts
    for i in 0..8u32 {
        ptr::write_volatile((0xE000_E180 + i * 4) as *mut u32, 0xFFFF_FFFF); // ICER
        ptr::write_volatile((0xE000_E280 + i * 4) as *mut u32, 0xFFFF_FFFF); // ICPR
    }

    // Enable SYSCFG clock for memory remapping
    reg_modify(RCC, RCC_APB2ENR, 0, 1 << 14); // SYSCFGEN
    let _ = reg_read(RCC, RCC_APB2ENR);

    // Remap system memory to 0x00000000
    ptr::write_volatile(SYSCFG as *mut u32, 0x01); // SYSCFG_MEMRMP = System Flash

    // Read SP and reset handler from system memory vector table
    let sp = ptr::read_volatile(0x1FFF_0000 as *const u32);
    let pc = ptr::read_volatile(0x1FFF_0004 as *const u32);

    // Set stack pointer and jump
    cortex_m::register::msp::write(sp);
    let jump: extern "C" fn() -> ! = core::mem::transmute(pc);
    jump();
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

// ════════════════════════════════════════════════════════════════
// Clock configuration: HSE 16 MHz → PLL → 96 MHz SYSCLK
// ════════════════════════════════════════════════════════════════

unsafe fn init_clocks() {
    // Flash wait states: 3 WS for 96 MHz @ 3.3V
    reg_modify(FLASH_R, FLASH_ACR, 0xF, 3);

    // Enable HSE oscillator (16 MHz crystal)
    reg_modify(RCC, RCC_CR, 0, 1 << 16); // HSEON
    while reg_read(RCC, RCC_CR) & (1 << 17) == 0 {} // wait HSERDY

    // Configure PLL:
    //   PLLM = 16 → VCO input = 16 MHz / 16 = 1 MHz
    //   PLLN = 192 → VCO output = 1 MHz × 192 = 192 MHz
    //   PLLP = 0 (÷2) → SYSCLK = 192 / 2 = 96 MHz
    //   PLLQ = 4 → USB clock = 192 / 4 = 48 MHz
    //   PLLSRC = HSE
    reg_write(
        RCC,
        RCC_PLLCFGR,
        16 | (192 << 6) | (0 << 16) | (1 << 22) | (4 << 24),
    );

    // Enable PLL
    reg_modify(RCC, RCC_CR, 0, 1 << 24); // PLLON
    while reg_read(RCC, RCC_CR) & (1 << 25) == 0 {} // wait PLLRDY

    // Bus prescalers: AHB=÷1, APB1=÷2 (48 MHz max), APB2=÷1
    //   HPRE  [7:4]   = 0000 (÷1)
    //   PPRE1 [12:10]  = 100  (÷2)
    //   PPRE2 [15:13]  = 000  (÷1)
    reg_modify(RCC, RCC_CFGR, 0xFCF0, 0x1000);

    // Switch system clock to PLL: SW [1:0] = 10
    reg_modify(RCC, RCC_CFGR, 0x3, 0x2);
    while (reg_read(RCC, RCC_CFGR) & 0xC) != 0x8 {} // wait SWS = PLL
}

// ════════════════════════════════════════════════════════════════
// Power and GPIO initialization
// ════════════════════════════════════════════════════════════════

unsafe fn init_power_and_gpio() {
    // Enable GPIOA and GPIOE clocks
    reg_modify(RCC, RCC_AHB1ENR, 0, (1 << 0) | (1 << 4)); // GPIOAEN | GPIOEEN

    // Small delay for clock to stabilize
    let _ = reg_read(RCC, RCC_AHB1ENR);
    let _ = reg_read(RCC, RCC_AHB1ENR);

    // PA13 → output push-pull, HIGH (keep power on!)
    // After reset PA13 is AF mode (SWDIO). Switch to output (01).
    reg_modify(GPIOA, GPIO_MODER, 3 << 26, 1 << 26);
    reg_write(GPIOA, GPIO_BSRR, 1 << 13); // set HIGH

    // PA14 → output push-pull, HIGH (enable I/O port VCC)
    reg_modify(GPIOA, GPIO_MODER, 3 << 28, 1 << 28);
    reg_write(GPIOA, GPIO_BSRR, 1 << 14); // set HIGH

    // Configure PE9, PE11, PE13, PE14 alternate function = AF1 (TIM1)
    // AFRH: pin 9 bits [7:4], pin 11 bits [15:12], pin 13 bits [23:20], pin 14 bits [27:24]
    let af_clear = (0xF << 4) | (0xF << 12) | (0xF << 20) | (0xF << 24);
    let af_set = (1 << 4) | (1 << 12) | (1 << 20) | (1 << 24); // AF1
    reg_modify(GPIOE, GPIO_AFRH, af_clear, af_set);

    // Set motor pins to high speed
    let speed_mask = (3 << 18) | (3 << 22) | (3 << 26) | (3 << 28);
    let speed_val = (3 << 18) | (3 << 22) | (3 << 26) | (3 << 28); // very high
    reg_modify(GPIOE, GPIO_OSPEEDR, speed_mask, speed_val);

    // Start with all motor pins as GPIO output LOW (coast = safe default)
    let moder_mask = (3u32 << 18) | (3 << 22) | (3 << 26) | (3 << 28);
    let moder_out = (1u32 << 18) | (1 << 22) | (1 << 26) | (1 << 28);
    reg_modify(GPIOE, GPIO_MODER, moder_mask, moder_out);
    // All LOW via BSRR reset bits (bits 16+pin)
    reg_write(GPIOE, GPIO_BSRR, (1 << 25) | (1 << 27) | (1 << 29) | (1 << 30));
}

// ════════════════════════════════════════════════════════════════
// TIM1 setup: 12 kHz PWM on channels 1–4, inverted polarity
// ════════════════════════════════════════════════════════════════

unsafe fn init_timer() {
    // Enable TIM1 clock (APB2)
    reg_modify(RCC, RCC_APB2ENR, 0, 1 << 0); // TIM1EN
    let _ = reg_read(RCC, RCC_APB2ENR);

    // Prescaler: 96 MHz / 8 = 12 MHz timer clock
    reg_write(TIM1, TIM_PSC, 7); // PSC = 8-1

    // Period: 12 MHz / 1000 = 12 kHz PWM
    reg_write(TIM1, TIM_ARR, 999); // ARR = 1000-1

    // CCMR1: CH1+CH2 in PWM mode 1 with preload
    //   OC1M [6:4] = 110 (PWM mode 1), OC1PE [3] = 1
    //   OC2M [14:12] = 110, OC2PE [11] = 1
    reg_write(TIM1, TIM_CCMR1, 0x6868);

    // CCMR2: CH3+CH4 same
    reg_write(TIM1, TIM_CCMR2, 0x6868);

    // CCER: enable all 4 channels, inverted polarity
    //   CC1E=1, CC1P=1, CC2E=1, CC2P=1, CC3E=1, CC3P=1, CC4E=1, CC4P=1
    reg_write(TIM1, TIM_CCER, 0x3333);

    // Start with CCR = 0 on all channels
    reg_write(TIM1, TIM_CCR1, 0);
    reg_write(TIM1, TIM_CCR2, 0);
    reg_write(TIM1, TIM_CCR3, 0);
    reg_write(TIM1, TIM_CCR4, 0);

    // BDTR: Main Output Enable (required for TIM1 advanced timer)
    reg_write(TIM1, TIM_BDTR, 1 << 15); // MOE

    // CR1: enable counter + auto-reload preload
    reg_write(TIM1, TIM_CR1, (1 << 7) | (1 << 0)); // ARPE | CEN

    // Force update to load prescaler and ARR
    reg_write(TIM1, TIM_EGR, 1); // UG
}

// ════════════════════════════════════════════════════════════════
// Motor control API
// ════════════════════════════════════════════════════════════════

/// Which motor port to control
#[derive(Clone, Copy)]
pub enum Motor {
    /// Port A: TIM1 CH1 (PE9) + CH2 (PE11)
    A,
    /// Port B: TIM1 CH3 (PE13) + CH4 (PE14)
    B,
}

impl Motor {
    /// Returns (pin1_bit, pin2_bit, moder_pin1_shift, moder_pin2_shift, ccr1_offset, ccr2_offset)
    const fn hw(self) -> (u32, u32, u32, u32, u32, u32) {
        match self {
            Motor::A => (9, 11, 18, 22, TIM_CCR1, TIM_CCR2),
            Motor::B => (13, 14, 26, 28, TIM_CCR3, TIM_CCR4),
        }
    }
}

/// Set motor speed. `duty` ranges from -1000 (full reverse) to +1000 (full forward).
/// 0 = electrical brake.
pub fn motor_set(motor: Motor, duty: i16) {
    let duty = duty.clamp(-1000, 1000);
    let (pin1, pin2, sh1, sh2, ccr1, ccr2) = motor.hw();

    unsafe {
        if duty > 0 {
            // Forward: pin1 = AF/PWM, pin2 = GPIO HIGH
            reg_write(TIM1, ccr1, duty as u32);
            // pin1 → AF mode (10), pin2 → output mode (01)
            reg_modify(GPIOE, GPIO_MODER, (3 << sh1) | (3 << sh2), (2 << sh1) | (1 << sh2));
            reg_write(GPIOE, GPIO_BSRR, 1 << pin2); // pin2 HIGH
        } else if duty < 0 {
            // Reverse: pin1 = GPIO HIGH, pin2 = AF/PWM
            reg_write(TIM1, ccr2, (-duty) as u32);
            // pin1 → output mode (01), pin2 → AF mode (10)
            reg_modify(GPIOE, GPIO_MODER, (3 << sh1) | (3 << sh2), (1 << sh1) | (2 << sh2));
            reg_write(GPIOE, GPIO_BSRR, 1 << pin1); // pin1 HIGH
        } else {
            // Brake: both GPIO HIGH
            reg_modify(GPIOE, GPIO_MODER, (3 << sh1) | (3 << sh2), (1 << sh1) | (1 << sh2));
            reg_write(GPIOE, GPIO_BSRR, (1 << pin1) | (1 << pin2));
        }
    }
}

/// Coast (free-wheel): both H-bridge pins LOW, motor spins freely.
pub fn motor_coast(motor: Motor) {
    let (pin1, pin2, sh1, sh2, _, _) = motor.hw();
    unsafe {
        // Both pins → output mode (01)
        reg_modify(GPIOE, GPIO_MODER, (3 << sh1) | (3 << sh2), (1 << sh1) | (1 << sh2));
        // Both LOW via BSRR reset bits
        reg_write(GPIOE, GPIO_BSRR, (1 << (pin1 + 16)) | (1 << (pin2 + 16)));
    }
}

// ════════════════════════════════════════════════════════════════
// Timing
// ════════════════════════════════════════════════════════════════

/// Busy-wait delay in milliseconds (at 96 MHz).
fn delay_ms(ms: u32) {
    // cortex_m::asm::delay does `n` iterations of a 1-cycle loop.
    // At 96 MHz, 96_000 iterations ≈ 1 ms.
    for _ in 0..ms {
        cortex_m::asm::delay(96_000);
    }
}

/// Check if USB cable is connected via VBUS sensing on PA9.
/// PA9 is configured as input by default after reset (MODER=00).
/// GPIOA clock must already be enabled.
fn usb_vbus_detected() -> bool {
    unsafe { reg_read(GPIOA, GPIO_IDR) & (1 << 9) != 0 }
}

// ════════════════════════════════════════════════════════════════
// LED matrix: 5×5 display via TLC5955 LED driver on SPI1
//
// TLC5955: 48-channel PWM LED driver with 769-bit shift register.
//   SPI1: PA5 (SCK), PA6 (MISO), PA7 (MOSI) — AF5
//   LAT:  PA15 (GPIO output) — pulse HIGH→LOW to latch data
//   GSCLK: PB15 via TIM12_CH2 — 9.6 MHz (drives LED PWM refresh)
// ════════════════════════════════════════════════════════════════

const TLC5955_DATA_SIZE: usize = 97; // (769 + 7) / 8

/// TLC5955 channel index for each LED in the 5×5 matrix.
/// Index = row * 5 + col, value = TLC5955 channel number.
const LED_CHANNELS: [u8; 25] = [
    38, 36, 41, 46, 33, // Row 0
    37, 28, 39, 47, 21, // Row 1
    24, 29, 31, 45, 23, // Row 2
    26, 27, 32, 34, 22, // Row 3
    25, 40, 30, 35,  9, // Row 4
];

/// Control latch for TLC5955 (sent via SPI to configure the chip).
/// Settings: dot correction = 100%, max current = 3.2 mA,
/// global brightness = 100%, auto repeat = on, ES-PWM = on.
const CONTROL_LATCH: [u8; TLC5955_DATA_SIZE] = {
    let mut d = [0xFFu8; TLC5955_DATA_SIZE];
    d[0] = 0x01;  // bit 768 = 1 → control latch mode
    d[1] = 0x96;  // magic byte (required by TLC5955)
    let mut i = 2;
    while i < 50 { d[i] = 0x00; i += 1; } // padding zeros
    d[50] = 0x06; // lsdvlt=1, espwm=1
    d[51] = 0x7F; // dsprpt=1, bc[6:1]
    d[52] = 0xFF; // bc continued
    d[53] = 0xFE; // bc end, mc=0 start
    d[54] = 0x00; // mc=0 (3.2 mA all colors)
    // Bytes 55–96: dc=127 → all 0xFF (already set)
    d
};

/// Grayscale buffer: byte 0 = 0 (grayscale mode), then 48 channels × 16-bit.
static mut GRAYSCALE_BUF: [u8; TLC5955_DATA_SIZE] = [0u8; TLC5955_DATA_SIZE];

/// Blocking SPI1 transmit.
unsafe fn spi_send(data: &[u8]) {
    for &byte in data {
        while reg_read(SPI1, SPI_SR) & (1 << 1) == 0 {} // wait TXE
        reg_write(SPI1, SPI_DR, byte as u32);
    }
    while reg_read(SPI1, SPI_SR) & (1 << 1) == 0 {} // final TXE
    while reg_read(SPI1, SPI_SR) & (1 << 7) != 0 {} // wait BSY clear
    let _ = reg_read(SPI1, SPI_DR); // clear OVR
    let _ = reg_read(SPI1, SPI_SR);
}

/// Pulse LAT pin (PA15) HIGH→LOW to latch data into TLC5955.
unsafe fn led_latch() {
    reg_write(GPIOA, GPIO_BSRR, 1 << 15);        // PA15 HIGH
    reg_write(GPIOA, GPIO_BSRR, 1 << (15 + 16)); // PA15 LOW
}

/// Set brightness of one LED in the 5×5 matrix.
/// `index` = row * 5 + col (0–24), `brightness` = 0–100.
fn led_set_pixel(index: usize, brightness: u16) {
    if index >= 25 { return; }
    let ch = LED_CHANNELS[index] as usize;
    // Gamma correction: brightness² (matches pybricks)
    let duty = (u16::MAX as u32) * (brightness as u32) * (brightness as u32) / 10000;
    let duty = duty.min(u16::MAX as u32) as u16;
    unsafe {
        GRAYSCALE_BUF[ch * 2 + 1] = (duty >> 8) as u8;
        GRAYSCALE_BUF[ch * 2 + 2] = duty as u8;
    }
}

/// Send current grayscale buffer to TLC5955 and latch.
unsafe fn led_update() {
    spi_send(core::slice::from_raw_parts(
        &raw const GRAYSCALE_BUF as *const u8,
        TLC5955_DATA_SIZE,
    ));
    led_latch();
}

/// Initialize SPI1, TIM12 (GSCLK), and TLC5955 for the LED matrix.
unsafe fn init_led_matrix() {
    // ── Enable clocks ──
    reg_modify(RCC, RCC_AHB1ENR, 0, 1 << 1);  // GPIOBEN
    reg_modify(RCC, RCC_APB2ENR, 0, 1 << 12); // SPI1EN
    reg_modify(RCC, RCC_APB1ENR, 0, 1 << 6);  // TIM12EN
    let _ = reg_read(RCC, RCC_APB1ENR);

    // ── SPI1 pins: PA5 (SCK), PA6 (MISO), PA7 (MOSI) → AF5 ──
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

    // ── LAT pin: PA15 → output, start LOW ──
    reg_modify(GPIOA, GPIO_MODER, 3 << 30, 1 << 30);
    reg_write(GPIOA, GPIO_BSRR, 1 << (15 + 16));

    // ── GSCLK: PB15 → AF9 (TIM12_CH2) ──
    reg_modify(GPIOB, GPIO_MODER, 3 << 30, 2 << 30);
    reg_modify(GPIOB, GPIO_AFRH, 0xF << 28, 9 << 28);

    // ── SPI1: master, 8-bit, mode 0, MSB first, /4 = 24 MHz, software NSS ──
    reg_write(SPI1, SPI_CR1,
        (1 << 2) |  // MSTR
        (1 << 3) |  // BR[0] → prescaler 4
        (1 << 6) |  // SPE
        (1 << 8) |  // SSI
        (1 << 9)    // SSM
    );

    // ── TIM12 CH2: 9.6 MHz GSCLK (96 MHz APB1 timer clock / 10) ──
    reg_write(TIM12, TIM_PSC, 0);
    reg_write(TIM12, TIM_ARR, 9);
    reg_write(TIM12, TIM_CCR2, 4); // 50% duty
    reg_write(TIM12, TIM_CCMR1, (6 << 12) | (1 << 11)); // OC2M=PWM1, OC2PE
    reg_write(TIM12, TIM_CCER, 1 << 4); // CC2E
    reg_write(TIM12, TIM_EGR, 1);  // UG
    reg_write(TIM12, TIM_CR1, 1);  // CEN

    // ── Send control latch twice (max current needs 2 writes) ──
    spi_send(&CONTROL_LATCH);
    led_latch();
    spi_send(&CONTROL_LATCH);
    led_latch();
}

// ════════════════════════════════════════════════════════════════
// Main: demo loop — spin both motors forward, reverse, coast
// ════════════════════════════════════════════════════════════════

#[entry]
fn main() -> ! {
    unsafe {
        // ── Check for DFU request (magic RAM value from previous run) ──
        // Must be FIRST — before power hold, before clocks, before anything.
        // If we find the magic value, we jump to STM32 system bootloader
        // and never return. PA13 will float low → hub stays powered only
        // while USB is connected (which it is during dev.sh flash cycle).
        check_and_enter_dfu();

        // ── CRITICAL: hold power BEFORE anything else ──
        // PA13 LOW = hub powers off. Must set HIGH immediately.
        // Enable GPIOA clock first (minimal register access).
        reg_modify(RCC, RCC_AHB1ENR, 0, 1 << 0); // GPIOAEN
        let _ = reg_read(RCC, RCC_AHB1ENR); // wait for clock
        reg_modify(GPIOA, GPIO_MODER, 3 << 26, 1 << 26); // PA13 output
        reg_write(GPIOA, GPIO_BSRR, 1 << 13); // PA13 HIGH = keep power

        // Now safe to do full init
        init_clocks();
        init_power_and_gpio();
        init_timer();
        init_led_matrix();
    }

    // Show heart on LED matrix — hub is alive!
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

    // Start safe
    motor_coast(Motor::A);
    motor_coast(Motor::B);
    delay_ms(500);

    // Quick kick on both motors so you know the code is running
    // (short pulse — won't move much, but you'll feel/hear it)
    motor_set(Motor::A, 400);
    motor_set(Motor::B, 400);
    delay_ms(150);
    motor_coast(Motor::A);
    motor_coast(Motor::B);
    delay_ms(500);
    motor_set(Motor::A, -400);
    motor_set(Motor::B, -400);
    delay_ms(150);
    motor_coast(Motor::A);
    motor_coast(Motor::B);
    delay_ms(1000);

    // Check if USB is connected — determines dev mode vs standalone mode.
    // PA9 (USB VBUS) is input after reset; GPIOA clock is already enabled.
    // Ensure PA9 is plain input (not alternate function from LEGO bootloader).
    unsafe { reg_modify(GPIOA, GPIO_MODER, 3 << 18, 0) }; // PA9 = input (00)

    let dev_mode = usb_vbus_detected();

    loop {
        // ── Forward ramp ──────────────────────────────
        // Smoothly ramp both motors from 0 to 600 (60% speed)
        for duty in (0..=600).step_by(10) {
            motor_set(Motor::A, duty);
            motor_set(Motor::B, duty);
            delay_ms(5);
        }
        delay_ms(2000); // hold at speed

        // ── Slow down ─────────────────────────────────
        for duty in (0..=600).rev().step_by(10) {
            motor_set(Motor::A, duty as i16);
            motor_set(Motor::B, duty as i16);
            delay_ms(5);
        }

        // ── Brake briefly ─────────────────────────────
        motor_set(Motor::A, 0);
        motor_set(Motor::B, 0);
        delay_ms(500);

        // ── Reverse ramp ──────────────────────────────
        for duty in (0..=600).step_by(10) {
            motor_set(Motor::A, -(duty as i16));
            motor_set(Motor::B, -(duty as i16));
            delay_ms(5);
        }
        delay_ms(2000);

        // ── Slow down ─────────────────────────────────
        for duty in (0..=600).rev().step_by(10) {
            motor_set(Motor::A, -(duty as i16));
            motor_set(Motor::B, -(duty as i16));
            delay_ms(5);
        }

        // ── Coast and pause ───────────────────────────
        motor_coast(Motor::A);
        motor_coast(Motor::B);
        delay_ms(2000);

        // ── Dev mode: re-enter DFU after one demo cycle ──
        // When USB is connected (development), automatically jump to
        // STM32 system bootloader DFU so dev.sh can flash the next build.
        // When running on battery only, loop forever.
        if dev_mode {
            motor_coast(Motor::A);
            motor_coast(Motor::B);
            request_dfu(); // writes magic to RAM + resets → ST DFU
        }
    }
}
