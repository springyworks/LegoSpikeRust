/*
 * glue_sim.c — Stubs and glue for running pbio natively on Linux.
 *
 * Provides:
 * - OS hooks (signal-based IRQ disable/enable, pselect-based WFI)
 * - HMI stubs (immediate program start, no UI)
 * - Bluetooth stubs
 * - Reset/power stubs (exit() instead of hardware power off)
 * - Stack driver stub
 * - pbsys_main callbacks (delegates to Rust user program)
 * - main() entry point
 */

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <signal.h>
#include <string.h>
#include <sys/select.h>
#include <unistd.h>

#include "pbio_os_config.h"
#include <pbio/error.h>
#include <pbio/os.h>
#include <pbio/button.h>
#include <pbio/protocol.h>
#include <pbsys/main.h>
#include <pbsys/status.h>
#include <pbsys/hmi.h>

/* ======== OS hooks (native signal-based) ======== */

pbio_os_irq_flags_t pbio_os_hook_disable_irq(void) {
    sigset_t sigmask;
    sigfillset(&sigmask);
    sigset_t origmask;
    pthread_sigmask(SIG_SETMASK, &sigmask, &origmask);
    return origmask;
}

void pbio_os_hook_enable_irq(pbio_os_irq_flags_t flags) {
    sigset_t origmask = (sigset_t)flags;
    pthread_sigmask(SIG_SETMASK, &origmask, NULL);
}

void pbio_os_hook_wait_for_interrupt(pbio_os_irq_flags_t flags) {
    struct timespec timeout = {
        .tv_sec = 0,
        .tv_nsec = 1000000, /* 1ms — matches SysTick on real hardware */
    };
    sigset_t origmask = flags;
    pselect(0, NULL, NULL, NULL, &timeout, &origmask);
    pbio_os_request_poll();
}

/* ======== Stack driver stub (native has no linker-controlled stack) ======== */

static uint8_t fake_stack[64 * 1024];
uint8_t *pbdrv_stack_start = fake_stack;
uint8_t *pbdrv_stack_end = fake_stack + sizeof(fake_stack);

/* ======== Storage heap (for PBSYS_CONFIG_STORAGE=0 inline stub) ======== */

/* Linker-like symbols: taken by address in storage.h inline stubs.
 * We just need two uint8_t symbols whose addresses bracket a heap region. */
static uint8_t storage_heap[64 * 1024];
uint8_t pbsys_storage_heap_start __attribute__((section(".data")));
uint8_t pbsys_storage_heap_end __attribute__((section(".data")));

/* ======== Bluetooth stubs ======== */

void pbdrv_bluetooth_start_advertising(void) {}
void pbdrv_bluetooth_stop_advertising(void) {}
bool pbdrv_bluetooth_is_connected(uint8_t type) { (void)type; return false; }
bool pbdrv_bluetooth_host_is_connected(void) { return false; }
void pbdrv_bluetooth_send(uint8_t *data, uint32_t size) { (void)data; (void)size; }
bool pbdrv_bluetooth_is_ready(void) { return false; }
void pbdrv_bluetooth_init(void) {}
void pbdrv_bluetooth_deinit(void) {}
pbio_error_t pbdrv_bluetooth_set_hub_name(const char *name, uint8_t len) {
    (void)name; (void)len; return 0;
}
void pbdrv_bluetooth_set_receive_handler(void *handler) { (void)handler; }
void pbdrv_bluetooth_set_host_connection_changed_callback(void *callback) { (void)callback; }
void pbdrv_bluetooth_close_user_tasks(void) {}
void pbdrv_bluetooth_schedule_status_update(void) {}
int pbdrv_bluetooth_await_advertise_or_scan_command(void *pt) { (void)pt; return 3; }

/* ======== HMI — immediate program start ======== */

void pbsys_hmi_init(void) {
    printf("[spike-rt-sim] HMI init — will auto-start program\n");
}

void pbsys_hmi_deinit(void) {}

void pbsys_hmi_stop_animation(void) {}

pbio_error_t pbsys_hmi_await_program_selection(void) {
    /* Let processes run briefly to finish initialization */
    for (int i = 0; i < 10; i++) {
        pbio_os_run_processes_and_wait_for_event();
    }

    printf("[spike-rt-sim] Starting user program...\n");
    return pbsys_main_program_request_start(
        PBIO_PYBRICKS_USER_PROGRAM_ID_REPL,
        PBSYS_MAIN_PROGRAM_START_REQUEST_TYPE_BOOT);
}

/* ======== pbsys_main callbacks ======== */

extern void spike_rt_user_program(void);

void pbsys_main_run_program(pbsys_main_program_t *program) {
    (void)program;
    spike_rt_user_program();
}

void pbsys_main_stop_program(bool force_stop) {
    (void)force_stop;
}

pbio_error_t pbsys_main_program_validate(pbsys_main_program_t *program) {
    (void)program;
    return 0;
}

void pbsys_main_run_program_cleanup(void) {}

const char *pbsys_main_get_application_version_hash(void) {
    return "spike-sim";
}

bool pbsys_main_stdin_event(uint8_t byte) {
    (void)byte;
    return false;
}

/* ======== Reset/power stubs ======== */

void pbdrv_reset_init(void) {}

void pbdrv_reset(int action) {
    printf("[spike-rt-sim] Reset requested (action=%d), exiting.\n", action);
    exit(0);
}

void pbdrv_reset_power_off(void) {
    printf("[spike-rt-sim] Power off requested, exiting.\n");
    exit(0);
}

int pbdrv_reset_get_reason(void) {
    return 0;
}

/* ======== Watchdog stub ======== */

void pbdrv_watchdog_init(void) {}
void pbdrv_watchdog_update(void) {}
