// Minimal self-contained Mode 6 (hi-res) offset-per-tile test ROM.
// BG1 = vertical 4-colour bars (red/green/blue/white, colour = tile
// column & 3 via palette group). BG3 OPT gives screen columns >= 16 a
// +16 H-offset (BG1 enable bit set). Correct emulation shifts the right
// half's colour sequence by ONE 16-px hi-res tile; a doubling bug shifts
// it by TWO.
arch snes.cpu
output "mode6opt.sfc", create

macro seek(variable offset) {
  origin ((offset & $7F0000) >> 1) | (offset & $7FFF)
  base offset
}

seek($8000); fill $8000

seek($8000)
Start:
  sei
  clc
  xce            // native mode
  sep #$20       // A 8-bit
  rep #$10       // X/Y 16-bit
  ldx.w #$1FFF
  txs

  lda.b #$80
  sta.w $2100    // force blank

  // palette -> CGRAM
  stz.w $2121    // CGADD = 0
  stz.w $4300    // byte, increment
  lda.b #$22
  sta.w $4301    // -> $2122 CGDATA
  ldx.w #Pal
  stx.w $4302
  lda.b #Pal>>16
  sta.w $4304
  ldx.w #128
  stx.w $4305
  lda.b #$01
  sta.w $420B

  lda.b #$80
  sta.w $2115    // VMAIN: inc after high byte

  // tiles -> VRAM word $0000
  ldx.w #$0000
  stx.w $2116
  lda.b #$01
  sta.w $4300    // word, increment
  lda.b #$18
  sta.w $4301    // -> $2118 VMDATA
  ldx.w #Tiles
  stx.w $4302
  lda.b #Tiles>>16
  sta.w $4304
  ldx.w #64
  stx.w $4305
  lda.b #$01
  sta.w $420B

  // BG1 map -> VRAM word $1000
  ldx.w #$1000
  stx.w $2116
  lda.b #$01
  sta.w $4300
  lda.b #$18
  sta.w $4301
  ldx.w #BG1Map
  stx.w $4302
  lda.b #BG1Map>>16
  sta.w $4304
  ldx.w #2048
  stx.w $4305
  lda.b #$01
  sta.w $420B

  // BG3 OPT map -> VRAM word $1400
  ldx.w #$1400
  stx.w $2116
  lda.b #$01
  sta.w $4300
  lda.b #$18
  sta.w $4301
  ldx.w #BG3Map
  stx.w $4302
  lda.b #BG3Map>>16
  sta.w $4304
  ldx.w #2048
  stx.w $4305
  lda.b #$01
  sta.w $420B

  // registers
  lda.b #$06
  sta.w $2105    // BG Mode 6
  stz.w $2133    // SETINI = 0 (no interlace)
  lda.b #$10
  sta.w $2107    // BG1SC: map word $1000, 32x32
  lda.b #$14
  sta.w $2109    // BG3SC: map word $1400, 32x32
  stz.w $210B    // BG12NBA
  stz.w $210C    // BG34NBA
  lda.b #$01
  sta.w $212C    // TM: BG1 main
  sta.w $212D    // TS: BG1 sub
  stz.w $210D
  stz.w $210D    // BG1 H = 0
  stz.w $210E
  stz.w $210E    // BG1 V = 0
  stz.w $2111
  stz.w $2111    // BG3 H = 0
  stz.w $2112
  stz.w $2112    // BG3 V = 0

  lda.b #$0F
  sta.w $2100    // screen on

Loop:
  jmp Loop

Pal:
  insert "pal.bin"
Tiles:
  insert "tiles.bin"
BG1Map:
  insert "bg1map.bin"
BG3Map:
  insert "bg3map.bin"

seek($FFC0)
  db "MODE6 OPT TEST       "  // 21-byte title
seek($FFFC)
  dw Start                    // emulation reset vector
