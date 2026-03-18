// spike-rt-sim platform config — minimal native simulation
// Based on virtual_hub but without bluetooth, display, rproc

#define PBDRV_CONFIG_BATTERY                                (1)
#define PBDRV_CONFIG_BATTERY_TEST                           (1)

#define PBDRV_CONFIG_BLOCK_DEVICE                           (0)

#define PBDRV_CONFIG_BLUETOOTH                              (0)

#define PBDRV_CONFIG_BUTTON                                 (1)
#define PBDRV_CONFIG_BUTTON_TEST                            (1)

#define PBDRV_CONFIG_CLOCK                                  (1)
#define PBDRV_CONFIG_CLOCK_LINUX                            (1)

#define PBDRV_CONFIG_COUNTER                                (1)

#define PBDRV_CONFIG_DISPLAY                                (0)

#define PBDRV_CONFIG_GPIO                                   (1)
#define PBDRV_CONFIG_GPIO_VIRTUAL                           (1)

#define PBDRV_CONFIG_IOPORT                                 (1)
#define PBDRV_CONFIG_IOPORT_NUM_DEV                         (6)

#define PBDRV_CONFIG_MOTOR_DRIVER                           (1)
#define PBDRV_CONFIG_MOTOR_DRIVER_NUM_DEV                   (6)
#define PBDRV_CONFIG_MOTOR_DRIVER_VIRTUAL_SIMULATION        (1)

#define PBDRV_CONFIG_HAS_PORT_A (1)
#define PBDRV_CONFIG_HAS_PORT_B (1)
#define PBDRV_CONFIG_HAS_PORT_C (1)
#define PBDRV_CONFIG_HAS_PORT_D (1)
#define PBDRV_CONFIG_HAS_PORT_E (1)
#define PBDRV_CONFIG_HAS_PORT_F (1)
#define PBDRV_CONFIG_HAS_PORT_VCC_CONTROL                   (1)

#define PBDRV_CONFIG_RPROC                                  (0)

#define PBDRV_CONFIG_USB                                    (0)
