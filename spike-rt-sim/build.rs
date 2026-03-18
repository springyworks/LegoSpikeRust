// build.rs — Compiles the Pybricks pbio C library for native x86_64 simulation
use std::path::PathBuf;

fn main() {
    let pybricks = PathBuf::from("../pybricks-micropython");
    let pbio = pybricks.join("lib/pbio");
    let lego = pybricks.join("lib/lego");
    let lwrb = pybricks.join("lib/lwrb");
    let contiki = pybricks.join("lib/contiki-core");

    // Include paths — our csrc/ FIRST so our pbdrvconfig.h/pbsysconfig.h/pbio_os_config.h
    // take priority over the platform-specific ones
    let include_dirs: Vec<PathBuf> = vec![
        // Our platform config (pbdrvconfig.h, pbsysconfig.h, pbio_os_config.h)
        "csrc".into(),
        // pbio public API
        pbio.join("include"),
        // pbio private headers
        pbio.clone(),
        // LEGO protocol definitions
        lego.clone(),
        // lwrb ring buffer
        lwrb.join("src/include"),
        // contiki-core (sys/process.h etc)
        contiki.clone(),
    ];

    // ===== Collect C source files =====
    let mut c_sources: Vec<PathBuf> = Vec::new();

    // --- pbio OS & core ---
    for f in &[
        "src/os.c",
        "src/main.c",
        "src/error.c",
        "src/util.c",
        "src/busy_count.c",
        "src/debug.c",
        "src/battery.c",
        "src/int_math.c",
    ] {
        c_sources.push(pbio.join(f));
    }

    // --- pbio motor system ---
    for f in &[
        "src/motor_process.c",
        "src/dcmotor.c",
        "src/servo.c",
        "src/control.c",
        "src/control_settings.c",
        "src/trajectory.c",
        "src/integrator.c",
        "src/observer.c",
        "src/differentiator.c",
        "src/angle.c",
        "src/tacho.c",
        "src/drivebase.c",
        "src/geometry.c",
        "src/parent.c",
        "src/motor/servo_settings.c",
    ] {
        c_sources.push(pbio.join(f));
    }

    // --- pbio port system ---
    for f in &["src/port.c"] {
        c_sources.push(pbio.join(f));
    }

    // --- pbio light system ---
    for f in &[
        "src/light/animation.c",
        "src/light/color_light.c",
        "src/light/light_matrix.c",
        "src/color/conversion.c",
        "src/color/util.c",
        "src/image/image.c",
        "src/image/font_mono_8x5_8.c",
    ] {
        c_sources.push(pbio.join(f));
    }

    // --- pbio IMU (stubs / disabled via config) ---
    c_sources.push(pbio.join("src/imu.c"));

    // --- pbio protocol ---
    for f in &[
        "src/protocol/pybricks.c",
        "src/protocol/nus.c",
    ] {
        c_sources.push(pbio.join(f));
    }

    // --- pbio logger ---
    c_sources.push(pbio.join("src/logger.c"));

    // --- Virtual/test drivers ---
    for f in &[
        "drv/core.c",
        "drv/battery/battery_test.c",
        "drv/button/button_test.c",
        "drv/clock/clock_linux.c",
        "drv/gpio/gpio_virtual.c",
        "drv/motor_driver/motor_driver_virtual_simulation.c",
        "drv/ioport/ioport.c",
        "drv/led/led_core.c",
        "drv/led/led_array.c",
        "drv/pwm/pwm_core.c",
        "drv/pwm/pwm_test.c",
        "drv/random/random_adc.c",
    ] {
        c_sources.push(pbio.join(f));
    }

    // --- pbio system services ---
    // NOTE: hmi_none.c provides our HMI — but we override in glue_sim.c,
    // so we skip all hmi_*.c files. host.c, storage.c guarded by config=0.
    for f in &[
        "sys/main.c",
        "sys/core.c",
        "sys/battery.c",
        "sys/battery_temp.c",
        "sys/command.c",
        "sys/program_stop.c",
        "sys/status.c",
        "sys/storage_settings.c",
    ] {
        c_sources.push(pbio.join(f));
    }

    // --- LEGO device spec ---
    c_sources.push(lego.join("device.c"));

    // --- lwrb ring buffer ---
    c_sources.push(lwrb.join("src/lwrb/lwrb.c"));

    // --- Our glue code ---
    c_sources.push("csrc/glue_sim.c".into());
    c_sources.push("csrc/platform_sim.c".into());

    // ===== Build configuration =====
    let mut build = cc::Build::new();

    build
        .opt_level_str("2")
        .flag("-std=c11")
        .flag("-ffunction-sections")
        .flag("-fdata-sections")
        .flag("-fshort-enums")
        .flag("-D_GNU_SOURCE")
        .define("NDEBUG", None)
        .define("PBDRV_CONFIG_BLUETOOTH", Some("0"))
        .define("PBDRV_CONFIG_RESET", Some("1"))
        .define("SPIKE_RT_NO_MICROPYTHON", Some("1"))
        .warnings(false);

    // Add include directories
    for inc in &include_dirs {
        build.include(inc);
    }

    // Add source files
    for src in &c_sources {
        if src.exists() {
            build.file(src);
        } else {
            println!("cargo:warning=Missing C source: {}", src.display());
        }
    }

    build.compile("pbio_sim");

    println!("cargo:rustc-link-lib=static=pbio_sim");
    println!("cargo:rustc-link-lib=pthread");
    println!("cargo:rustc-link-lib=m");
}
