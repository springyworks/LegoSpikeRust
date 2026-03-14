/* Bootloader linker script for LEGO SPIKE Prime Hub
 * MCU: STM32F413VGT6 (1 MB Flash, 320 KB SRAM)
 *
 * Flash layout:
 *   0x08000000..0x08008000  LEGO DFU bootloader (32 KB, untouched)
 *   0x08008000..0x08010000  Our Rust bootloader (32 KB, this binary)
 *   0x08010000..0x08100000  Application firmware (960 KB)
 */
MEMORY
{
    FLASH : ORIGIN = 0x08008000, LENGTH = 32K
    RAM   : ORIGIN = 0x20000000, LENGTH = 320K
}
