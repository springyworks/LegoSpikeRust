//! Rust Bootloader for LEGO SPIKE Prime Hub (STM32F413VGT6)
//!
//! Flashed ONCE to 0x08008000 via LEGO DFU. After that, all app updates go
//! through the STM32 system bootloader (USB DFU device 0483:df11).
//!
//! Flash layout:
//!   0x08000000  LEGO DFU bootloader     (32 KB, factory, untouched)
//!   0x08008000  This Rust bootloader    (32 KB)
//!   0x08010000  Application firmware    (960 KB)
//!
//! Boot sequence:
//!   1. PA13 HIGH (power hold) — before anything else
//!   2. Check DFU magic in RAM — if set, enter STM32 system DFU
//!   3. Check center button (PC4 ADC) — if held at boot, enter DFU
//!   4. Validate app at 0x08010000 — if valid, jump to it
//!   5. If no valid app — enter STM32 system DFU
//!
//! Recovery: hold center button during power-on → enters DFU mode.
//! Flash app: dfu-util -d 0483:df11 -a 0 -s 0x08010000:leave -D app.bin

#![no_std]
#![no_main]

use core::ptr;

const APP_ADDR: u32 = 0x0801_0000;
const DFU_MAGIC_ADDR: *mut u32 = 0x2004_FFF0 as *mut u32;
const DFU_MAGIC_VALUE: u32 = 0xDEAD_B007;

// Register bases
const RCC: u32 = 0x4002_3800;
const GPIOA: u32 = 0x4002_0000;
#[allow(dead_code)]
const GPIOC: u32 = 0x4002_0800;
const ADC1: u32 = 0x4001_2000;
const SYSCFG: u32 = 0x4001_3800;

// Center button: PC4, ADC1 channel 14, threshold ≤ 2879 = pressed
const BUTTON_THRESHOLD: u32 = 2879;

// ════════════════════════════════════════════════════════════════
// PA13 power hold — runs BEFORE .bss/.data init (fastest possible)
// ════════════════════════════════════════════════════════════════

#[cortex_m_rt::pre_init]
unsafe fn pre_init() {
    // Enable GPIOA clock (RCC_AHB1ENR bit 0)
    let rcc_ahb1enr = (RCC + 0x30) as *mut u32;
    ptr::write_volatile(rcc_ahb1enr, ptr::read_volatile(rcc_ahb1enr) | 1);
    let _ = ptr::read_volatile(rcc_ahb1enr); // bus sync

    // PA13 → general-purpose output (MODER bits 27:26 = 01)
    let moder = GPIOA as *mut u32;
    let v = ptr::read_volatile(moder);
    ptr::write_volatile(moder, (v & !(3 << 26)) | (1 << 26));

    // PA13 HIGH (BSRR bit 13)
    ptr::write_volatile((GPIOA + 0x18) as *mut u32, 1 << 13);
}

// ════════════════════════════════════════════════════════════════
// Entry point
// ════════════════════════════════════════════════════════════════

#[cortex_m_rt::entry]
fn main() -> ! {
    unsafe {
        // ── 1. Check DFU magic ──────────────────────────────
        let magic = ptr::read_volatile(DFU_MAGIC_ADDR);
        if magic == DFU_MAGIC_VALUE {
            ptr::write_volatile(DFU_MAGIC_ADDR, 0);
            enter_system_dfu();
        }

        // ── 2. Check center button (held = enter DFU) ──────
        if read_center_button() {
            // Debounce: wait ~50ms at 16 MHz, check again
            busy_wait(800_000);
            if read_center_button() {
                enter_system_dfu();
            }
        }

        // ── 3. Validate application ─────────────────────────
        let app_sp = ptr::read_volatile(APP_ADDR as *const u32);
        let app_pc = ptr::read_volatile((APP_ADDR + 4) as *const u32);

        let sp_valid = app_sp >= 0x2000_0000 && app_sp <= 0x2005_0000;
        let pc_valid = app_pc >= APP_ADDR && app_pc <= 0x0810_0000;

        if sp_valid && pc_valid {
            jump_to_app(app_sp, app_pc);
        }

        // ── 4. No valid app → DFU ───────────────────────────
        enter_system_dfu();
    }
}

// ════════════════════════════════════════════════════════════════
// Center button via ADC1 channel 14 (PC4)
// ════════════════════════════════════════════════════════════════

unsafe fn read_center_button() -> bool {
    // Enable GPIOC clock (RCC_AHB1ENR bit 2)
    let rcc_ahb1enr = (RCC + 0x30) as *mut u32;
    ptr::write_volatile(rcc_ahb1enr, ptr::read_volatile(rcc_ahb1enr) | (1 << 2));

    // Enable ADC1 clock (RCC_APB2ENR bit 8)
    let rcc_apb2enr = (RCC + 0x44) as *mut u32;
    ptr::write_volatile(rcc_apb2enr, ptr::read_volatile(rcc_apb2enr) | (1 << 8));
    let _ = ptr::read_volatile(rcc_apb2enr);

    // PC4 is analog by default after reset — no GPIO config needed

    // ADC1: single conversion, channel 14, 480-cycle sample time
    ptr::write_volatile((ADC1 + 0x2C) as *mut u32, 0);        // SQR1: 1 conversion
    ptr::write_volatile((ADC1 + 0x34) as *mut u32, 14);       // SQR3: channel 14
    ptr::write_volatile((ADC1 + 0x0C) as *mut u32, 7 << 12);  // SMPR1: 480 cycles ch14

    // Power on ADC
    ptr::write_volatile((ADC1 + 0x08) as *mut u32, 1);        // CR2: ADON
    busy_wait(160_000); // ~10ms stabilization at 16 MHz

    // Start conversion
    let cr2 = ptr::read_volatile((ADC1 + 0x08) as *const u32);
    ptr::write_volatile((ADC1 + 0x08) as *mut u32, cr2 | (1 << 30)); // SWSTART

    // Wait for end-of-conversion (SR bit 1 = EOC)
    while ptr::read_volatile((ADC1 + 0x00) as *const u32) & (1 << 1) == 0 {}

    let val = ptr::read_volatile((ADC1 + 0x4C) as *const u32);

    // Power off ADC to save power
    ptr::write_volatile((ADC1 + 0x08) as *mut u32, 0);

    val <= BUTTON_THRESHOLD
}

// ════════════════════════════════════════════════════════════════
// Jump to application at APP_ADDR
// ════════════════════════════════════════════════════════════════

unsafe fn jump_to_app(sp: u32, pc: u32) -> ! {
    // Disable interrupts
    cortex_m::interrupt::disable();

    // Disable SysTick (LEGO bootloader may have left it running)
    ptr::write_volatile(0xE000_E010 as *mut u32, 0);

    // Clear any pending SysTick interrupt
    ptr::write_volatile(0xE000_ED04 as *mut u32, 1 << 25); // SCB_ICSR PENDSTCLR

    // Set VTOR to application vector table
    ptr::write_volatile(0xE000_ED08 as *mut u32, APP_ADDR);

    // Set MSP to application's initial stack pointer
    core::arch::asm!("MSR MSP, {}", in(reg) sp);

    // Jump to application reset handler
    let entry: extern "C" fn() -> ! = core::mem::transmute(pc);
    entry();
}

// ════════════════════════════════════════════════════════════════
// Enter STM32 system bootloader (built-in USB DFU at 0x1FFF0000)
// ════════════════════════════════════════════════════════════════

unsafe fn enter_system_dfu() -> ! {
    cortex_m::interrupt::disable();

    // Disable SysTick
    ptr::write_volatile(0xE000_E010 as *mut u32, 0);

    // Disable all NVIC interrupts and clear pending
    for i in 0..8u32 {
        ptr::write_volatile((0xE000_E180 + i * 4) as *mut u32, 0xFFFF_FFFF);
        ptr::write_volatile((0xE000_E280 + i * 4) as *mut u32, 0xFFFF_FFFF);
    }

    // Enable SYSCFG clock (RCC_APB2ENR bit 14)
    let rcc_apb2enr = (RCC + 0x44) as *mut u32;
    ptr::write_volatile(rcc_apb2enr, ptr::read_volatile(rcc_apb2enr) | (1 << 14));
    let _ = ptr::read_volatile(rcc_apb2enr);

    // Remap system memory to 0x00000000 (SYSCFG_MEMRMP = 0x01)
    ptr::write_volatile(SYSCFG as *mut u32, 0x01);

    // Load SP and PC from system bootloader vector table
    let sp = ptr::read_volatile(0x1FFF_0000 as *const u32);
    let pc = ptr::read_volatile(0x1FFF_0004 as *const u32);

    // Set MSP
    core::arch::asm!("MSR MSP, {}", in(reg) sp);

    // Jump to system bootloader
    let entry: extern "C" fn() -> ! = core::mem::transmute(pc);
    entry();
}

// ════════════════════════════════════════════════════════════════
// Helpers
// ════════════════════════════════════════════════════════════════

#[inline(never)]
fn busy_wait(cycles: u32) {
    for _ in 0..cycles {
        cortex_m::asm::nop();
    }
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    // Bootloader panic → enter DFU as recovery
    unsafe { enter_system_dfu() }
}
