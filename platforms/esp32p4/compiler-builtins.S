.section .text.__bswapsi2,"ax",@progbits
.global __bswapsi2
.type __bswapsi2,@function
__bswapsi2:
    srli a1, a0, 24
    srli a2, a0, 8
    andi a2, a2, 255
    slli a2, a2, 16
    or a1, a1, a2
    srli a2, a0, 16
    andi a2, a2, 255
    slli a2, a2, 8
    or a1, a1, a2
    slli a0, a0, 24
    or a0, a0, a1
    ret
.size __bswapsi2, .-__bswapsi2

.section .text.__popcountsi2,"ax",@progbits
.global __popcountsi2
.type __popcountsi2,@function
__popcountsi2:
    li a1, 0
1:
    beqz a0, 2f
    addi a2, a0, -1
    and a0, a0, a2
    addi a1, a1, 1
    j 1b
2:
    mv a0, a1
    ret
.size __popcountsi2, .-__popcountsi2

.section .data.__stack_chk_guard,"aw",@progbits
.global __stack_chk_guard
.type __stack_chk_guard,@object
.p2align 2
__stack_chk_guard:
    .word 0x6d5a56da
.size __stack_chk_guard, .-__stack_chk_guard

.section .text._sbrk,"ax",@progbits
.global _sbrk
.type _sbrk,@function
_sbrk:
    li a0, -1
    ret
.size _sbrk, .-_sbrk

.section .text.rust_eh_personality,"ax",@progbits
.global rust_eh_personality
.type rust_eh_personality,@function
rust_eh_personality:
    ret
.size rust_eh_personality, .-rust_eh_personality
