/* Synthetic polling fixtures; no SDK, ROM calls, or user firmware.
 * Existing classic model's MMIO layout, not a new hardware claim.
 * Compile with -DGPIO_FIXTURE for GPIO; otherwise byte-sink I2C polling.
 */
.section .text
.global _start
_start:
    movi a2, 0x3ff40000
    movi a3, 71
    s32i a3, a2, 0
#ifdef GPIO_FIXTURE
    movi a2, 0x3ff44000
    movi a3, 16
1:
    s32i a3, a2, 8
    l32i a4, a2, 0x44
    l32i a4, a2, 0x68
    s32i a3, a2, 12
    s32i a3, a2, 0x48
    l32i a4, a2, 0x68
    s32i a3, a2, 0x4c
    j 1b
#else
    movi a2, 0x3ff53000
1:
    movi a3, -1
    s32i a3, a2, 0x24
    movi a3, 0x78
    s32i a3, a2, 0x1c
    movi a3, 0x40
    s32i a3, a2, 0x1c
    movi a3, 0xa5
    s32i a3, a2, 0x1c
    movi a3, 0
    s32i a3, a2, 0x58
    movi a3, 0x903
    s32i a3, a2, 0x5c
    movi a3, 0x1800
    s32i a3, a2, 0x60
    movi a3, 32
    s32i a3, a2, 4
2:
    l32i a3, a2, 0x20
    beqz a3, 2b
    j 1b
#endif
