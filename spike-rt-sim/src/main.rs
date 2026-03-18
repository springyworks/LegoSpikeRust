//! spike-rt-sim: Native Linux simulator for LEGO SPIKE Prime Hub
//!
//! Runs the same pbio C library with virtual/test drivers on your PC.
//! Motors are physics-simulated, clock uses real Linux time.

mod ffi;

/// User program entry point — called from C (glue_sim.c → pbsys_main → here)
#[no_mangle]
pub extern "C" fn spike_rt_user_program() {
    println!("[Rust] spike_rt_user_program() starting!");

    let ms = unsafe { ffi::pbdrv_clock_get_ms() };
    println!("[Rust] Clock: {}ms since boot", ms);

    let buttons = unsafe { ffi::pbdrv_button_get_pressed() };
    println!("[Rust] Buttons: 0x{:x}", buttons);

    // Get motor driver for port A (index 0) and port B (index 1)
    let mut drv_a: *mut ffi::PbdrvMotorDriverDev = core::ptr::null_mut();
    let mut drv_b: *mut ffi::PbdrvMotorDriverDev = core::ptr::null_mut();
    unsafe {
        let err_a = ffi::pbdrv_motor_driver_get_dev(0, &mut drv_a);
        let err_b = ffi::pbdrv_motor_driver_get_dev(1, &mut drv_b);
        println!("[Rust] Motor driver A: err={}, B: err={}", err_a, err_b);
    }

    if !drv_a.is_null() && !drv_b.is_null() {
        // Use raw motor driver — set duty cycle directly (range: -10000 to +10000)
        unsafe {
            ffi::pbdrv_motor_driver_set_duty_cycle(drv_a, 7000); // ~70% forward
            ffi::pbdrv_motor_driver_set_duty_cycle(drv_b, -5000); // ~50% reverse
        }
        println!("[Rust] Motor A: duty=+7000, Motor B: duty=-5000");

        // Run physics simulation for ~500 ticks and read encoder angles
        // Get counter devices for angle readback
        let mut counter_a: *mut ffi::PbdrvCounterDev = core::ptr::null_mut();
        let mut counter_b: *mut ffi::PbdrvCounterDev = core::ptr::null_mut();
        unsafe {
            let ea = ffi::pbdrv_counter_get_dev(0, &mut counter_a);
            let eb = ffi::pbdrv_counter_get_dev(1, &mut counter_b);
            println!("[Rust] Counter A: err={}, ptr_null={} | B: err={}, ptr_null={}",
                ea, counter_a.is_null(), eb, counter_b.is_null());
        }

        for i in 0..500 {
            unsafe {
                usleep(1000); // 1ms — let the virtual motor physics advance in real time
                ffi::pbio_os_request_poll(); // signal that timer expired so processes actually run
                ffi::pbio_os_run_processes_once();
            }
            if i % 100 == 0 {
                let ms = unsafe { ffi::pbdrv_clock_get_ms() };
                let (mut rot_a, mut mdeg_a) = (0i32, 0i32);
                let (mut rot_b, mut mdeg_b) = (0i32, 0i32);
                let mut ea = -99i32;
                let mut eb = -99i32;
                if !counter_a.is_null() {
                    ea = unsafe { ffi::pbdrv_counter_get_angle(counter_a, &mut rot_a, &mut mdeg_a) };
                }
                if !counter_b.is_null() {
                    eb = unsafe { ffi::pbdrv_counter_get_angle(counter_b, &mut rot_b, &mut mdeg_b) };
                }
                println!("[Rust] Tick {} | clock: {}ms | A: {}rot+{}mdeg (e{}) | B: {}rot+{}mdeg (e{})",
                    i, ms, rot_a, mdeg_a, ea, rot_b, mdeg_b, eb);
            }
        }

        // Coast motors
        unsafe {
            ffi::pbdrv_motor_driver_coast(drv_a);
            ffi::pbdrv_motor_driver_coast(drv_b);
        }
        println!("[Rust] Motors coasted, simulation done.");
    } else {
        println!("[Rust] Could not get motor drivers, skipping motor test.");
        for i in 0..100 {
            unsafe { ffi::pbio_os_run_processes_once(); }
            if i % 25 == 0 {
                let ms = unsafe { ffi::pbdrv_clock_get_ms() };
                println!("[Rust] Tick {} — clock: {}ms", i, ms);
            }
        }
    }

    println!("[Rust] spike_rt_user_program() done!");
}

extern "C" {
    fn pbsys_main();
    fn usleep(usec: u32) -> i32;
}

fn main() {
    println!("[spike-rt-sim] LEGO SPIKE Prime Simulator");
    println!("[spike-rt-sim] Running pbsys_main()...");
    unsafe { pbsys_main(); }
    println!("[spike-rt-sim] pbsys_main() returned, exiting.");
}
