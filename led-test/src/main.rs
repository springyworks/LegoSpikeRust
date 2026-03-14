//! LED test binary for LEGO SPIKE Prime Hub (STM32F413VGT6)
//!
//! Experiments with the TLC5955 LED driver:
//!   - Status LED top/bottom (center button ring)
//!   - Battery LED
//!   - Bluetooth LED
//!   - 5×5 Light Matrix
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
const SPI1: u32 = 0x4001_3000;
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

/// Helper: set a single channel, send GS, wait
unsafe fn show_single(ch: usize, brightness: u16, ms: u32) {
    let mut gs = [0u16; 48];
    gs[ch] = brightness;
    tlc5955_send_gs(&gs);
    delay_ms(ms);
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

        let bright: u16 = 0x2000; // moderate brightness

        loop {
            // ── 1. Status LED top: Red → Green → Blue → White ──
            show_single(5, bright, 600);  // Red
            show_single(4, bright, 600);  // Green
            show_single(3, bright, 600);  // Blue

            // White (all three)
            let mut gs = [0u16; 48];
            gs[3] = bright; gs[4] = bright; gs[5] = bright;
            tlc5955_send_gs(&gs);
            delay_ms(600);

            // ── 2. Status LED bottom: Red → Green → Blue ───────
            show_single(8, bright, 500);  // Red
            show_single(7, bright, 500);  // Green
            show_single(6, bright, 500);  // Blue

            // ── 3. Both status LEDs: top=green, bottom=blue ────
            gs = [0; 48];
            gs[4] = bright; gs[6] = bright;
            tlc5955_send_gs(&gs);
            delay_ms(600);

            // ── 4. Battery LED: Red → Green → Blue ─────────────
            show_single(2, bright, 500);
            show_single(1, bright, 500);
            show_single(0, bright, 500);

            // ── 5. Bluetooth LED: Red → Green → Blue ───────────
            show_single(20, bright, 500);
            show_single(19, bright, 500);
            show_single(18, bright, 500);

            // ── 6. Light matrix: all on ────────────────────────
            gs = [0; 48];
            for &ch in &MATRIX {
                gs[ch as usize] = bright / 4;
            }
            tlc5955_send_gs(&gs);
            delay_ms(800);

            // ── 7. Light matrix: row by row ────────────────────
            for row in 0..5 {
                gs = [0; 48];
                for col in 0..5 {
                    gs[MATRIX[row * 5 + col] as usize] = bright;
                }
                tlc5955_send_gs(&gs);
                delay_ms(300);
            }

            // ── 8. Light matrix: column by column ──────────────
            for col in 0..5 {
                gs = [0; 48];
                for row in 0..5 {
                    gs[MATRIX[row * 5 + col] as usize] = bright;
                }
                tlc5955_send_gs(&gs);
                delay_ms(300);
            }

            // ── 9. Light matrix: diagonal sweep ────────────────
            for diag in 0..9 {
                gs = [0; 48];
                for row in 0..5 {
                    let col = diag as i32 - row as i32;
                    if col >= 0 && col < 5 {
                        gs[MATRIX[row * 5 + col as usize] as usize] = bright;
                    }
                }
                tlc5955_send_gs(&gs);
                delay_ms(200);
            }

            // ── 10. All off briefly ────────────────────────────
            gs = [0; 48];
            tlc5955_send_gs(&gs);
            delay_ms(800);
        }
    }
}
