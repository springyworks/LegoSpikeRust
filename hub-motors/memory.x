/* Linker script for LEGO SPIKE Prime / Robot Inventor 51515 hub
 * MCU: STM32F413VGT6 (1 MB Flash, 320 KB SRAM)
 *
 * Flash layout:
 *   0x08000000..0x08008000  LEGO DFU bootloader  (32 KB, factory)
 *   0x08008000..0x08010000  Rust bootloader      (32 KB)
 *   0x08010000..0x08100000  Application (this)   (960 KB)
 */
MEMORY
{
    FLASH : ORIGIN = 0x08010000, LENGTH = 960K
    RAM   : ORIGIN = 0x20000000, LENGTH = 312K
}
