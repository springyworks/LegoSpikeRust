/* Monitor linker script for LEGO SPIKE Prime Hub
 * MCU: STM32F413VGT6 (1 MB Flash, 320 KB SRAM)
 *
 * Flash layout:
 *   0x08000000..0x08008000  LEGO DFU bootloader (32 KB, untouched)
 *   0x08008000..0x08010000  Monitor (this binary, 32 KB)
 *   0x08010000..0x08100000  Application firmware (960 KB)
 */
/* Monitor RAM: top 8K of SRAM, safe from app overwrites.
 * App uses 0x20000000..0x2004E000 (312K).
 * Monitor uses 0x2004E000..0x20050000 (8K) for statics + stack.
 * Trampoline pointer at 0x2004FFE0 (in this region).
 * DFU magic at 0x2004FFF0 (in this region).
 */
MEMORY
{
    FLASH : ORIGIN = 0x08008000, LENGTH = 32K
    RAM   : ORIGIN = 0x2004E000, LENGTH = 8K
}
