; lo.s - fixture source for fixture_lo.dbg
.segment "CODE"
; reset vector entry
Reset:
    sei             ; disable IRQs
    clc
    xce
@loop:
    bra @loop       ; spin
.segment "RODATA"
; a 16-byte table
Table:
    .byte 0,1,2,3,4,5,6,7,8,9,10,11,12,13,14,15
