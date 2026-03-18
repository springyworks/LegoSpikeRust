//! Motor test — random small steps on ports A and B
//!
//! Beeps on start, then motors do random short bursts
//! in random directions, independently. Center button = stop & coast.

#![no_std]
#![no_main]

use core::ptr;

// ── Monitor trampolines (breakpoints + center-button pause) ──

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

// ── Register bases ──

const RCC: u32 = 0x4002_3800;
const FLASH_R: u32 = 0x4002_3C00;
const GPIOA: u32 = 0x4002_0000;
const GPIOC: u32 = 0x4002_0800;
const GPIOE: u32 = 0x4002_1000;
const ADC1: u32 = 0x4001_2000;
const TIM1: u32 = 0x4001_0000;

const BUTTON_CENTER_THRESHOLD: u32 = 2879;

// ── Register helpers ──

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
    cortex_m::asm::delay(ms * 96_000);
}

// ── PA13 power hold (must run before .bss init) ──

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

// ── Clocks: 96 MHz from 16 MHz HSE (skip if monitor already set up) ──

unsafe fn init_clocks() {
    if (reg_read(RCC, 0x08) & 0xC) == 0x8 { return; }
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

// ── Beep (startup sound) ──

unsafe fn beep(duration_ms: u32, freq_hz: u32) {
    reg_modify(RCC, 0x30, 0, (1 << 0) | (1 << 2));
    let _ = reg_read(RCC, 0x30);
    reg_modify(GPIOC, 0x00, 3 << 20, 1 << 20);
    reg_write(GPIOC, 0x18, 1 << 10);
    reg_modify(GPIOA, 0x00, 3 << 8, 1 << 8);
    let half = 96_000_000 / (freq_hz * 2);
    let toggles = duration_ms * freq_hz * 2 / 1000;
    for _ in 0..toggles {
        reg_write(GPIOA, 0x18, 1 << 4);
        cortex_m::asm::delay(half);
        reg_write(GPIOA, 0x18, 1 << (4 + 16));
        cortex_m::asm::delay(half);
    }
    reg_write(GPIOC, 0x18, 1 << (10 + 16));
    reg_modify(GPIOA, 0x00, 3 << 8, 3 << 8);
}

// ── ADC for center button ──

unsafe fn init_button_adc() {
    reg_modify(RCC, 0x30, 0, (1 << 0) | (1 << 2));
    reg_modify(RCC, 0x44, 0, 1 << 8);
    let _ = reg_read(RCC, 0x44);
    reg_modify(GPIOC, 0x00, 3 << 8, 3 << 8); // PC4 analog
    reg_write(ADC1, 0x2C, 0);
    reg_modify(ADC1, 0x0C, 7 << 12, 7 << 12);
    reg_modify(ADC1, 0x08, 0, 1 << 0);
}

fn read_adc(channel: u32) -> u32 {
    unsafe {
        cortex_m::interrupt::disable();
        reg_write(ADC1, 0x34, channel);
        reg_modify(ADC1, 0x08, 0, 1 << 0);
        reg_write(ADC1, 0x00, 0);
        reg_modify(ADC1, 0x08, 0, 1 << 30);
        let mut timeout = 100_000u32;
        while reg_read(ADC1, 0x00) & (1 << 1) == 0 {
            timeout -= 1;
            if timeout == 0 {
                reg_modify(ADC1, 0x08, 1 << 0, 0);
                cortex_m::asm::delay(100);
                reg_modify(ADC1, 0x08, 0, 1 << 0);
                cortex_m::interrupt::enable();
                return 4095;
            }
        }
        let val = reg_read(ADC1, 0x4C);
        cortex_m::interrupt::enable();
        val
    }
}

fn center_pressed() -> bool {
    read_adc(14) <= BUTTON_CENTER_THRESHOLD
}

// ── Motor H-bridge (ports A+B via TIM1) ──

unsafe fn init_motor_hw() {
    reg_modify(RCC, 0x30, 0, (1 << 0) | (1 << 4)); // GPIOA + GPIOE
    reg_modify(RCC, 0x44, 0, 1 << 0); // TIM1
    let _ = reg_read(RCC, 0x44);

    // PA14 output HIGH: enable I/O port VCC
    reg_modify(GPIOA, 0x00, 3 << 28, 1 << 28);
    reg_write(GPIOA, 0x18, 1 << 14);

    // PE9/11/13/14 AF1 (TIM1 CH1..CH4)
    let af_clear = (0xF << 4) | (0xF << 12) | (0xF << 20) | (0xF << 24);
    let af_set = (1 << 4) | (1 << 12) | (1 << 20) | (1 << 24);
    reg_modify(GPIOE, 0x24, af_clear, af_set);

    // High speed
    let speed_mask = (3 << 18) | (3 << 22) | (3 << 26) | (3 << 28);
    reg_modify(GPIOE, 0x08, speed_mask, speed_mask);

    // Start as GPIO outputs (coast)
    let moder_mask = (3u32 << 18) | (3 << 22) | (3 << 26) | (3 << 28);
    let moder_out = (1u32 << 18) | (1 << 22) | (1 << 26) | (1 << 28);
    reg_modify(GPIOE, 0x00, moder_mask, moder_out);
    reg_write(GPIOE, 0x18,
        (1 << (9 + 16)) | (1 << (11 + 16)) | (1 << (13 + 16)) | (1 << (14 + 16)));

    // TIM1 PWM: 12 kHz, ARR=999, PSC=7
    reg_write(TIM1, 0x28, 7);
    reg_write(TIM1, 0x2C, 999);
    reg_write(TIM1, 0x18, 0x6868);
    reg_write(TIM1, 0x1C, 0x6868);
    reg_write(TIM1, 0x20, 0x3333);
    reg_write(TIM1, 0x34, 0);
    reg_write(TIM1, 0x38, 0);
    reg_write(TIM1, 0x3C, 0);
    reg_write(TIM1, 0x40, 0);
    reg_write(TIM1, 0x44, 1 << 15); // BDTR MOE
    reg_write(TIM1, 0x00, (1 << 7) | (1 << 0)); // CR1 ARPE+CEN
    reg_write(TIM1, 0x14, 1); // EGR UG
}

unsafe fn set_motor_a(duty: i16) {
    let duty = duty.clamp(-1000, 1000);
    if duty > 0 {
        reg_write(TIM1, 0x34, duty as u32);
        reg_modify(GPIOE, 0x00, (3 << 18) | (3 << 22), (2 << 18) | (1 << 22));
        reg_write(GPIOE, 0x18, 1 << 11);
    } else if duty < 0 {
        reg_write(TIM1, 0x38, (-duty) as u32);
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
        reg_write(TIM1, 0x3C, duty as u32);
        reg_modify(GPIOE, 0x00, (3 << 26) | (3 << 28), (2 << 26) | (1 << 28));
        reg_write(GPIOE, 0x18, 1 << 14);
    } else if duty < 0 {
        reg_write(TIM1, 0x40, (-duty) as u32);
        reg_modify(GPIOE, 0x00, (3 << 26) | (3 << 28), (1 << 26) | (2 << 28));
        reg_write(GPIOE, 0x18, 1 << 13);
    } else {
        reg_modify(GPIOE, 0x00, (3 << 26) | (3 << 28), (1 << 26) | (1 << 28));
        reg_write(GPIOE, 0x18, (1 << (13 + 16)) | (1 << (14 + 16)));
    }
}

unsafe fn motor_coast_all() {
    set_motor_a(0);
    set_motor_b(0);
}

// ── PRNG (xorshift32) ──

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

fn rng_seed() {
    unsafe {
        let systick_val = ptr::read_volatile(0xE000_E018 as *const u32);
        let adc_noise = read_adc(14) ^ read_adc(1);
        RNG_STATE = systick_val ^ adc_noise ^ 0xCAFE_BABE;
        for _ in 0..8 { rng_next(); }
    }
}

// ── Main: random motor steps ──

const DUTY: i16 = 500;            // 50% power
const LOOP_MS: u32 = 20;          // 20 ms tick
const MIN_WAIT: u32 = 20;         // 400 ms min between moves
const WAIT_RANGE: u32 = 40;       // +0..800 ms random gap
const MIN_ON: u32 = 3;            // 60 ms min pulse
const ON_RANGE: u32 = 7;          // +0..140 ms random (max ~200 ms ≈ ~120°)

#[cortex_m_rt::entry]
fn main() -> ! {
    unsafe {
        init_clocks();
        beep(100, 1200); // low beep = "motor test starting"

        init_button_adc();
        init_motor_hw();
        rng_seed();

        let mut countdown_a: u32 = 5;  // frames until next move for A
        let mut countdown_b: u32 = 8;  // offset B so they don't always sync
        let mut on_a: u32 = 0;         // remaining on-frames for A
        let mut on_b: u32 = 0;         // remaining on-frames for B

        loop {
            // Center button = stop & return to monitor
            if center_pressed() {
                motor_coast_all();
                beep(60, 800);
                // Return to monitor — just loop with WFI (monitor's
                // center-button handler will catch this via SysTick)
                loop { cortex_m::asm::wfi(); }
            }

            // Motor A
            if on_a > 0 {
                on_a -= 1;
                if on_a == 0 { set_motor_a(0); }
            } else if countdown_a == 0 {
                let r = rng_next();
                let dur = MIN_ON + (r % (ON_RANGE + 1));
                let dir = if r & 0x80 != 0 { DUTY } else { -DUTY };
                set_motor_a(dir);
                on_a = dur;
                countdown_a = MIN_WAIT + (rng_next() % (WAIT_RANGE + 1));
            } else {
                countdown_a -= 1;
            }

            // Motor B
            if on_b > 0 {
                on_b -= 1;
                if on_b == 0 { set_motor_b(0); }
            } else if countdown_b == 0 {
                let r = rng_next();
                let dur = MIN_ON + (r % (ON_RANGE + 1));
                let dir = if r & 0x80 != 0 { DUTY } else { -DUTY };
                set_motor_b(dir);
                on_b = dur;
                countdown_b = MIN_WAIT + (rng_next() % (WAIT_RANGE + 1));
            } else {
                countdown_b -= 1;
            }

            delay_ms(LOOP_MS);
        }
    }
}
