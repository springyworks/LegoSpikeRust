//! Raw FFI bindings to the pbio C library (native sim version).

pub type PbioError = i32;

#[allow(dead_code)]
pub const PBIO_SUCCESS: PbioError = 0;

pub type PbioButtonFlags = u32;

#[allow(dead_code)]
pub const PBIO_BUTTON_CENTER: PbioButtonFlags = 1 << 5;
#[allow(dead_code)]
pub const PBIO_BUTTON_LEFT: PbioButtonFlags = 1 << 4;
#[allow(dead_code)]
pub const PBIO_BUTTON_RIGHT: PbioButtonFlags = 1 << 6;

/// Opaque motor driver handle (C struct pointer)
#[repr(C)]
pub struct PbdrvMotorDriverDev { _private: [u8; 0] }

/// Opaque counter device handle
#[repr(C)]
pub struct PbdrvCounterDev { _private: [u8; 0] }

extern "C" {
    pub fn pbdrv_clock_get_ms() -> u32;
    pub fn pbdrv_button_get_pressed() -> PbioButtonFlags;
    pub fn pbio_os_run_processes_once() -> bool;
    pub fn pbio_os_request_poll();

    // Motor driver (low level)
    pub fn pbdrv_motor_driver_get_dev(id: u8, driver: *mut *mut PbdrvMotorDriverDev) -> PbioError;
    pub fn pbdrv_motor_driver_set_duty_cycle(driver: *mut PbdrvMotorDriverDev, duty: i32) -> PbioError;
    pub fn pbdrv_motor_driver_coast(driver: *mut PbdrvMotorDriverDev) -> PbioError;

    // Counter (encoder angle reading)
    pub fn pbdrv_counter_get_dev(id: u8, dev: *mut *mut PbdrvCounterDev) -> PbioError;
    pub fn pbdrv_counter_get_angle(dev: *mut PbdrvCounterDev, rotations: *mut i32, millidegrees: *mut i32) -> PbioError;
}
