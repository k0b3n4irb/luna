# SuperFX / GSU (Graphics Support Unit) — Implementation Reference Spec

Diff target for the luna Rust port. This is the faithful-port reference for
luna's GSU: every claim describes hardware-accurate behaviour, verified against
the hardware reference.

The GSU core is the abstract CPU; a SoC wrapper supplies the system glue
(stepping, memory reads/writes, the pixel-plot path, the operand pipeline).
luna mirrors this split: a pure GSU instruction engine plus a SuperFX glue
layer that owns the memory map, timing, MMIO, and SNES handshake.

---

## 1. Register model

### 1.1 General-purpose registers R0–R15

16 × 16-bit registers (`Register r[16]`). Each `Register` tracks a `modified`
flag set on every write; the wrapper uses it to detect writes to R14 and R15.

Special-purpose roles:

| Reg | Role |
|---|---|
| R0  | default source & dest after `reset()` (`sreg=dreg=0`) |
| R1  | PLOT/RPIX X coordinate |
| R2  | PLOT/RPIX Y coordinate |
| R3  | (general) |
| R4  | LMULT low-16 result destination |
| R6  | FMULT/LMULT implicit multiplicand |
| R7  | MERGE high source (`r7 & 0xff00`) |
| R8  | MERGE low source (`r8 >> 8`) |
| R11 | LINK destination (return address) |
| R12 | LOOP counter (decremented) |
| R13 | LOOP branch target |
| R14 | **ROM-load trigger**: writing R14 starts a ROM buffer fetch |
| R15 | **Program counter** |

**R14 ROM-load trigger.** After any instruction, if `regs.r[14].modified` the
wrapper calls `updateROMBuffer()`. Also on a direct MMIO write to R14 from the
SNES. `updateROMBuffer()` sets `sfr.r=1` and arms `romcl`. The fetch at
`(rombr<<16)+r14` lands `romcl` cycles later. Equivalently, on a write to
register 14 the status flag `RomReadPending` is set and `RomDelay = ClockSelect ? 5 : 6`.

**R15 PC autoincrement.** After executing an instruction, if R15 was *not* written
by it, R15 is incremented. Branch / JMP / LOOP write R15 explicitly to suppress
the increment.

### 1.2 SFR — Status Flag Register (16-bit, MMIO $3030/$3031)

Bit layout:

| Bit | Name | Meaning |
|---|---|---|
| 0  | — | (always 0) |
| 1  | z   | zero flag |
| 2  | cy  | carry flag |
| 3  | s   | sign flag |
| 4  | ov  | overflow flag |
| 5  | g   | **go** flag (GSU running) |
| 6  | r   | ROM-read pending (R14 fetch in flight) |
| 7  | — | (always 0) |
| 8  | alt1 | ALT1 prefix mode active |
| 9  | alt2 | ALT2 prefix mode active |
| 10 | il  | immediate-lower latch |
| 11 | ih  | immediate-upper latch |
| 12 | b   | "with" (B) prefix flag |
| 13–14 | — | (always 0) |
| 15 | irq | interrupt asserted to SNES |

Composite `alt` = bits 8:9 — the 2-bit ALT mode selector {alt0,alt1,alt2,alt3}.

**Read mask:** SFR reads back as `data & 0x9f7e` — i.e. bits {1,2,3,4,5,6,
8,9,10,11,12, 15} are visible; bits 0,7,13,14 read 0. Built explicitly: low byte =
z<<1|cy<<2|s<<3|ov<<4|g<<5|r<<6; high byte = alt1<<0|alt2<<1|il<<2|ih<<3|b<<4|irq<<7.
The low byte omits bit 0 and bit 7 (0x7e low / 0x9f high).

### 1.3 PBR / ROMBR / RAMBR / CBR / SCBR

| Reg | Width | Role | Reset |
|---|---|---|---|
| pbr   | n8   | Program Bank Register — bank for PC fetches; ≤$5F = ROM, ≥$60 = RAM | 0x00 |
| rombr | n8   | ROM data Bank Register — bank for R14 ROM buffer fetch / GETx | 0x00 |
| rambr | bool | RAM data Bank Register — 1-bit RAM bank for LD/ST buffer ops | 0 |
| cbr   | n16  | Cache Base Register — base PC of the 512-byte instruction cache window; only bits 15:4 meaningful (`&0xfff0`) | 0x0000 |
| scbr  | n8   | Screen Base Register — Game Pak RAM tile-map base, used as `scbr<<10` | 0x00 |
| colr  | n8   | Color Register (PLOT color) | 0x00 |

`pbr` writable range is masked to 7 bits on MMIO write (`data & 0x7f`) and on
LJMP (`r[n] & 0x7f`). `rombr` masked 7-bit. `rambr` masked 1-bit.

### 1.4 SCMR — Screen Mode Register ($303A) — **scrambled bit layout**

Fields:
- `ht` (height select, **2 bits split across two non-adjacent SCMR bits**)
- `ron` (1 = GSU has ROM access; SNES locked out)
- `ran` (1 = GSU has RAM access; SNES locked out)
- `md`  (color depth mode, 2 bits)

The wire byte ↔ field mapping is deliberately scrambled. **Write** `scmr = data`:
```
ht  = ((data & 0x20) ? 1 : 0) << 1   // ht bit1 <- byte bit5
ht |= ((data & 0x04) ? 1 : 0) << 0   // ht bit0 <- byte bit2
ron = data & 0x10                    // byte bit4
ran = data & 0x08                    // byte bit3
md  = data & 0x03                    // byte bits1:0
```
**Read** reverses it:
```
byte = ((ht>>1)<<5) | (ron<<4) | (ran<<3) | ((ht&1)<<2) | md
```
Byte bit map: `bit5=ht_hi, bit4=ron, bit3=ran, bit2=ht_lo, bits1:0=md`.

`md` → bpp mapping: md∈{0,1,2,3} → bpp∈{2,4,4,8}. Computed as
`bpp = 2 << (md - (md>>1))`. md==3 (8bpp) is the "OBJ-capable" /
transparency-special mode.

### 1.5 POR — Plot Option Register (set by CMODE, $4E alt1)

Fields, set from source register low byte:

| Bit | Field | Meaning |
|---|---|---|
| 0 | transparent | 1 = plot even transparent (color 0) pixels |
| 1 | dither | 1 = dither color by (x^y)&1 (non-8bpp only) |
| 2 | highnibble | color() returns `(colr&0xf0)|(src>>4)` |
| 3 | freezehigh | color() returns `(colr&0xf0)|(src&0x0f)`; also gates md==3 transparency on low nibble |
| 4 | obj  | OBJ mode — forces tile-index calc to ht==3 layout |

### 1.6 CFGR — Config Register ($3037)

Fields:
- `irq` (bit 7): 1 = **mask** GSU IRQ (STOP will not raise IRQ)
- `ms0` (bit 5): 1 = high-speed multiply (MULT/FMULT timing)

`cfgr = data` → `irq=data&0x80; ms0=data&0x20`.

### 1.7 Other single-bit / misc registers

- `bramr` (bool) — Backup-RAM write enable ($3033, `data&0x01`); when 0, SNES
  writes to BRAM are dropped.
- `vcr` (n8) — Version Code Register, read-only at $303B, reset 0x04.
- `clsr` (bool) — Clock Select Register ($3039, `data&0x01`): 0 = 10.7 MHz,
  1 = 21.4 MHz. Affects every cycle cost (`clsr ? fast : slow`).
- `pipeline` (n8) — the prefetched opcode (the 1-stage pipe). Reset to 0x01 (NOP)
  on power.
- `ramaddr` (n16) — last RAM address used by LD/ST (for SBK).

### 1.8 Reset / power state

All R[]=0, sfr=0, pbr=rombr=0, rambr=0, cbr=0, scbr=0, scmr=0, colr=0, por=0,
bramr=0, **vcr=0x04**, cfgr=0, clsr=0, pipeline=0x01 (NOP), ramaddr=0.
`reset()` also zeroes sfr.b/alt1/alt2 and sreg=dreg=0.
Wrapper power: cache.buffer all 0, all 32 valid flags false, pixelcache[0/1].offset=~0,
bitpend=0, romcl=romdr=ramcl=ramar=ramdr=0.

### 1.9 MMIO address map ($3000–$303F, cache $3100–$32FF)

SNES sees these at `$00-$3F:$3000-$34FF` (see §3).
Internally `address = 0x3000 | addr.bit(0,9)` — mirrors every 0x400.

| Addr | R/W | Function |
|---|---|---|
| $3000–$301F | R/W | R0–R15, little-endian byte pairs: reg = `addr>>1 & 15`, byte = `addr&1` |
| $301F (W high byte of R15) | W | **also sets sfr.g=1 (GO)** — writing R15 high byte launches the GSU |
| $3030 | R | SFR low byte |
| $3030 | W | SFR low byte; if g 1→0, cbr=0 + flushCache |
| $3031 | R | SFR high byte; **side effect: clears sfr.irq and deasserts CPU IRQ** |
| $3031 | W | SFR high byte |
| $3033 | W | BRAMR (`data&0x01`) |
| $3034 | R | PBR |
| $3034 | W | PBR (`data&0x7f`) + flushCache |
| $3036 | R | ROMBR |
| $3037 | W | CFGR |
| $3038 | W | SCBR |
| $3039 | W | CLSR (`data&0x01`) |
| $303A | W | SCMR (scrambled, §1.4) |
| $303B | R | VCR (0x04) |
| $303C | R | RAMBR |
| $303E | R | CBR low byte |
| $303F | R | CBR high byte |
| $3100–$32FF | R/W | instruction cache RAM (512B), indexed `(addr-0x3100+cbr)&511` |
| (other) | R | 0x00 |

Every MMIO access first synchronizes the coprocessor up to the SNES clock before
the access.

**MMIO access-gating edge case:** on real hardware most MMIO is blocked while the
GSU is running — reads return 0 for all but $3030/$3031/$303B when the GSU is
running; writes allow only $3030/$303A while running ("During GSU operation, only
SFR, SCMR, and VCR may be accessed"). luna does not require this lockout for
correctness of the games tested; it is documented as a hardware nicety. Serve all
addresses unconditionally.

---

## 2. Instruction set

### 2.1 The ALT prefix mechanism (CRITICAL)

The same opcode byte decodes to a different instruction depending on `sfr.alt1` /
`sfr.alt2`. The 2-bit value `alt2<<1 | alt1` selects one of four "modes":

| alt2 alt1 | mode | set by |
|---|---|---|
| 0 0 | ALT0 (default) | — |
| 0 1 | ALT1 | `$3D ALT1` |
| 1 0 | ALT2 | `$3E ALT2` |
| 1 1 | ALT3 | `$3F ALT3` |

- `$3D ALT1`: `sfr.b=0; sfr.alt1=1`
- `$3E ALT2`: `sfr.b=0; sfr.alt2=1`
- `$3F ALT3`: `sfr.b=0; sfr.alt1=1; sfr.alt2=1`

ALT1/2/3 are themselves prefix instructions: they do **not** call `reset()`, so the
alt bits survive into the *next* instruction. Every *non-prefix* instruction ends
with `regs.reset()` which clears b, alt1, alt2 and resets sreg/dreg to 0.
Thus alt flags are consumed by exactly one following op.

The prefix ops TO/FROM/WITH set sreg/dreg and the `b` ("with") flag (§2.2) and also
**do not** call reset() in their prefix role.

### 2.2 Register-select prefixes: TO / FROM / WITH and the B flag

These build the source/dest operand selection for the *next* ALU/move op:

- `$B0–$BF FROM rN` (ALT0, b=0): `sreg = n` (set source).
- `$10–$1F TO rN` (ALT0, b=0): `dreg = n` (set dest).
- `$20–$2F WITH rN`: `sreg = n; dreg = n; sfr.b = 1`.

The `b` ("with") flag changes TO/FROM into the *move* variants:

- `$10–$1F MOVE rN` (b=1): `r[n] = sr()` then reset().
- `$B0–$BF MOVES rN` (b=1): `dr() = r[n]`, sets ov/s/z, then reset().

So: a bare `FROM rA` / `TO rB` only *latches* sreg/dreg and the next op uses them
(this is how `ADD rN` reads `sr()` as src and writes `dr()` as dest). A `WITH rA`
sets both and the `b` flag, making the immediately following `TO`/`FROM` act as
register-to-register MOVE/MOVES. After any non-prefix op, reset() restores sreg=dreg=0
(so the implicit operand defaults back to R0).

`sr()` = `r[sreg]`, `dr()` = `r[dreg]`.

### 2.3 The pipeline / operand fetch

`pipe()` returns the prefetched byte `regs.pipeline` and prefetches the next opcode
from `++r15`. `peekpipe()` returns pipeline but prefetches from the *current* r15
without incrementing the returned value's basis; the main loop uses `peekpipe()` to
get the opcode. Operand bytes (branch displacement, immediates, RAM short/long
addresses) are read via `pipe()`. Equivalently: an opcode read followed by an
operand read that increments R15 then refills the program read buffer.

### 2.4 Opcode table

Notation: `sr` = `r[sreg]`, `dr` = `r[dreg]`, `n` = low nibble of opcode (register
index) or immediate as noted. "reset" = clears alt/b + sreg/dreg unless it's a
prefix op. Flag column lists flags written. Cycle cost is the *extra* cost beyond
the base fetch/decode; base instruction execution is implicitly the pipeline step
(see §6) — only ops that call `step()` themselves add the listed extra cycles.
`F` = fast-clock cycles (clsr=1), `S` = slow (clsr=0); where a single number is
shown the op adds no explicit step().

| Opcode | ALT0 (default) | ALT1 | ALT2 | ALT3 | Effect / notes |
|---|---|---|---|---|---|
| $00 | stop | stop | stop | stop | if `!cfgr.irq`: sfr.irq=1, raise CPU IRQ. sfr.g=0; pipeline=NOP; reset() |
| $01 | nop | nop | nop | nop | reset() only |
| $02 | cache | cache | cache | cache | if cbr≠(r15&0xfff0): cbr=r15&0xfff0, flushCache |
| $03 | lsr | lsr | lsr | lsr | cy=sr&1; dr=sr>>1; s,z |
| $04 | rol | rol | rol | rol | dr=(sr<<1)|cy_in; cy=sr&0x8000; s,z |
| $05 | bra e | (same all modes) | | | unconditional; disp=`(i8)pipe()`; if take r15+=disp |
| $06 | b** (see note) | | | | cond branch — `(s^ov)==0` |
| $07 | b** | | | | cond branch — `(s^ov)==1` |
| $08 | bne | | | | `z==0` |
| $09 | beq | | | | `z==1` |
| $0A | bpl | | | | `s==0` |
| $0B | bmi | | | | `s==1` |
| $0C | bcc | | | | `cy==0` |
| $0D | bcs | | | | `cy==1` |
| $0E | bvc | | | | `ov==0` |
| $0F | bvs | | | | `ov==1` |
| $10–$1F | to rN / move rN(b=1) | (same) | | | b=0: dreg=N; b=1: r[N]=sr, reset |
| $20–$2F | with rN | (same) | | | sreg=dreg=N; sfr.b=1 (prefix, no reset) |
| $30–$3B | stw (rN) | stb (rN) | stw | stb | ramaddr=r[N]; writeRAMBuffer(addr, sr); if !alt1 also (addr^1, sr>>8) |
| $3C | loop | loop | loop | loop | r12--; s,z from r12; if !z r15=r13 |
| $3D | alt1 | — | — | — | b=0; alt1=1 (prefix) |
| $3E | alt2 | — | — | — | b=0; alt2=1 (prefix) |
| $3F | alt3 | — | — | — | b=0; alt1=alt2=1 (prefix) |
| $40–$4B | ldw (rN) | ldb (rN) | ldw | ldb | ramaddr=r[N]; dr=readRAMBuffer(addr); if !alt1 dr|=readRAMBuffer(addr^1)<<8 |
| $4C | plot | rpix | plot | rpix | see §4. RPIX adds bpp×(F5/S6) step |
| $4D | swap | swap | swap | swap | dr=(sr>>8)|(sr<<8); s,z |
| $4E | color | cmode | color | cmode | alt0: colr=color(sr); alt1: por=sr (CMODE) |
| $4F | not | not | not | not | dr=~sr; s,z |
| $50–$5F | add rN | adc rN | add #N | adc #N | r=sr+op(+cy if alt1); ov,s,cy,z; dr=r |
| $60–$6F | sub rN | sbc rN | sub #N | cmp rN | r=sr-op(-!cy if sbc); ov,s,cy,z; dr=r (CMP: no write) |
| $70 | merge | merge | merge | merge | dr=(r7&0xff00)|(r8>>8); ov,s,cy,z special masks |
| $71–$7F | and rN | bic rN | and #N | bic #N | dr=sr & (alt1?~op:op); s,z |
| $80–$8F | mult rN | umult rN | mult #N | umult #N | signed/unsigned 8×8→16; s,z; if !ms0 add F1/S2 step |
| $90 | sbk | sbk | sbk | sbk | writeRAMBuffer(ramaddr,sr); (ramaddr^1, sr>>8) |
| $91–$94 | link #N | (same) | | | r11=r15+N (N=1..4 = low nibble) |
| $95 | sex | sex | sex | sex | dr=(i8)sr (sign-extend low byte); s,z |
| $96 | asr | div2 | asr | div2 | cy=sr&1; dr=(i16)sr>>1 (+rounding if div2); s,z |
| $97 | ror | ror | ror | ror | dr=(cy<<15)|(sr>>1); cy=sr&1; s,z |
| $98–$9D | jmp rN | ljmp rN | jmp | ljmp | alt0: r15=r[N]; alt1: pbr=r[N]&0x7f, r15=sr, cbr=r15&0xfff0, flushCache |
| $9E | lob | lob | lob | lob | dr=sr&0xff; s(bit7),z |
| $9F | fmult | lmult | fmult | lmult | (i16)sr*(i16)r6; dr=res>>16; cy=res&0x8000; if lmult r4=res; s,z; step (ms0?3:7)×(clsr?1:2) |
| $A0–$AF | ibt rN,#pp | lms rN,(yy) | sms (yy),rN | (=alt1 lms) | alt1: short-addr load; alt2: short-addr store; else r[N]=(i8)pipe() |
| $B0–$BF | from rN / moves(b=1) | (same) | | | b=0: sreg=N; b=1: dr=r[N], ov/s/z, reset |
| $C0 | hib | hib | hib | hib | dr=sr>>8; s(bit7),z |
| $C1–$CF | or rN | xor rN | or #N | xor #N | dr=sr|op or sr^op; s,z |
| $D0–$DE | inc rN | (same) | | | r[N]++; s,z |
| $DF | getc | (getc) | ramb | romb | alt0: colr=color(readROMBuffer); alt2&!alt1: syncRAM, rambr=sr&1; alt3: syncROM, rombr=sr&0x7f |
| $E0–$EE | dec rN | (same) | | | r[N]--; s,z |
| $EF | getb | getbh | getbl | getbs | ROM buffer byte → dr per alt2:alt1 (see below) |
| $F0–$FF | iwt rN,#xx | lm rN,(xx) | sm (xx),rN | (=alt1 lm) | alt1: long-addr load; alt2: long-addr store; else r[N]=word immediate |

**Branch-name $06/$07 caution.** The dispatch maps `$06 → Branch((s^ov)==0)` and
`$07 → Branch((s^ov)==1)`. Mnemonic labels for these two opcodes vary between
secondary references (one labels $06="blt"/$07="bge", another swaps them), but
**the branch *conditions* are identical** — for luna only the *condition per
opcode byte* matters: `$06 → take when (s^ov)==0`, `$07 → take when (s^ov)==1`.
Implement by condition, ignore the mnemonic dispute.

**GETB ($EF) sub-decode** (`alt2<<1|alt1`):
- 0 getb : `dr = romBuffer`
- 1 getbh: `dr = romBuffer<<8 | (n8)sr`
- 2 getbl: `dr = (sr&0xff00) | romBuffer`
- 3 getbs: `dr = (i8)romBuffer`

### 2.5 ALU flag details (exact)

- **ADD/ADC**: operand `n = (alt2 ? immediate-nibble : r[n])`;
  carry-in = `alt1 ? cy : 0`. `r = sr + n + carryin`.
  `ov = ~(sr^n) & (n^r) & 0x8000`; `s = r&0x8000`; `cy = r>=0x10000`; `z = (u16)r==0`.
- **SUB/SBC/CMP**: operand selection `n = (!alt2 || alt1) ? r[n]
  : immediate` — i.e. immediate only for SUB#N (alt2 && !alt1). Borrow term:
  `(!alt2 && alt1) ? !cy : 0` (SBC). `r = sr - n - borrow`.
  `ov = (sr^n) & (sr^r) & 0x8000`; `s = r&0x8000`; `cy = r>=0`; `z=(u16)r==0`.
  Write dr **unless** CMP (alt3 = alt2&&alt1): `if(!alt2 || !alt1) dr=r`.
  (The SBC borrow `result -= Carry ? 0 : 1` == `!cy`.)
- **AND/BIC**: `n = alt2 ? imm : r[n]`; `dr = sr & (alt1 ? ~n : n)`.
- **OR/XOR**: `n = alt2 ? imm : r[n]`; `dr = alt1 ? sr^n : sr|n`.
- **MERGE**: `dr=(r7&0xff00)|(r8>>8)`; flags use packed masks:
  `ov=dr&0xc0c0; s=dr&0x8080; cy=dr&0xe0e0; z=dr&0xf0f0` (note: z is *set* when any
  of those bits set — these are unusual "any-bit" flags, not standard z/s).
- **ASR/DIV2**: `cy=sr&1`; `dr = (i16)sr>>1 + (alt1 ? ((sr+1)>>16)
  : 0)`. The `(sr+1)>>16` rounding term is the DIV2 correction.
- **MULT/UMULT**: `n = alt2 ? imm : r[n]`;
  `dr = alt1 ? (u16)((u8)sr*(u8)n) : (u16)((i8)sr*(i8)n)` (8×8→16, low byte operands).
  Timing: if `!ms0` add `step(clsr?1:2)`.
- **FMULT/LMULT**: `result = (i16)sr * (i16)r6` (16×16→32);
  `dr = result>>16`; `cy = result&0x8000`; if LMULT `r4 = result` (low 16).
  Timing: `step((ms0?3:7) * (clsr?1:2))`.

### 2.6 LD / ST and RAM-buffer ops

- **STW/STB ($30, store)**: `ramaddr=r[N]; writeRAMBuffer(ramaddr, sr)`; STW also
  `writeRAMBuffer(ramaddr^1, sr>>8)`. writeRAMBuffer arms a delayed write (see §6).
- **LDW/LDB ($40, load)**: `ramaddr=r[N]; dr=readRAMBuffer(ramaddr)`; LDW also
  `dr |= readRAMBuffer(ramaddr^1)<<8`.
- **SBK ($90)**: re-store word to last `ramaddr`.
- **IBT/LMS/SMS ($A0)**: alt1 LMS load word from `(pipe()<<1)`; alt2 SMS store word to
  `(pipe()<<1)`; default IBT `r[N]=(i8)pipe()`.
- **IWT/LM/SM ($F0)**: alt1 LM load word from 16-bit immediate addr (two pipe bytes);
  alt2 SM store; default IWT load 16-bit immediate into r[N].

### 2.7 GETC / RAMB / ROMB ($DF) and GETB ($EF)

- GETC (alt0): `colr = color(readROMBuffer())`.
- RAMB (alt2,!alt1): syncRAMBuffer; `rambr = sr & 1`.
- ROMB (alt3): syncROMBuffer; `rombr = sr & 0x7f`.
- GETB family — see §2.4.

---

## 3. Memory map & bus

### 3.1 GSU-side view

The GSU's data bus decodes the 24-bit address:

| GSU address | Target | Gating |
|---|---|---|
| `$00–$3F:$0000–$FFFF` | ROM, mapped `((addr&0x3f0000)>>1)|(addr&0x7fff)` then `&romMask` | stalls while `!scmr.ron` |
| `$40–$5F:$0000–$FFFF` | ROM, linear `addr & romMask` | stalls while `!scmr.ron` |
| `$70–$71:$0000–$FFFF` | Game Pak RAM, `addr & ramMask` | stalls while `!scmr.ran` (read & write) |
| (other) | open bus → returns passed `data` | — |

ROM/RAM-access stall: while the relevant access bit (ron/ran) is *not* granted to the
GSU, the read/write loops `step(6); synchronize(cpu); break if scheduler.synchronizing()`
— i.e. the GSU burns cycles waiting for the SNES to grant access via SCMR. luna can
model this as: if access not granted, stall the GSU (yield to SNES) until SCMR flips
the bit.

The first-bank ROM remap `((addr&0x3f0000)>>1)|(addr&0x7fff)` packs the upper-half
$8000–$FFFF of each LoROM bank into a contiguous ROM image; the `$40–$5F` window is the
flat/linear view of the same ROM. `romMask = romSizeRound(rom.size())-1` (rounded up to
a power of two to fix the non-power-of-two SuperFX voxel demo).
`ramMask = ram.size()-1`, `bramMask = bram.size()-1`.

GSU-side mappings: `$00–$3F:$8000–$FFFF` ROM, `$00–$3F:$0000–$7FFF` ROM **mirror**,
`$40–$5F:$0000–$FFFF` ROM, `$70–$71:$0000–$FFFF` GSU RAM. (The $0000–$7FFF mirror is the
LoROM low-half mirror folded into the remap formula.)

### 3.2 SNES-CPU-side view (the SuperFX mapper)

Three memory shims are wired into the SNES bus; the SNES board glue places them. The
CPU-side address ranges:

| SNES address | Target |
|---|---|
| `$00–$3F:$3000–$3FFF` and `$80–$BF:$3000–$3FFF` | GSU MMIO |
| `$00–$3F:$8000–$FFFF`, `$80–$BF:$8000–$FFFF` | ROM |
| `$40–$5F:$0000–$FFFF`, `$C0–$DF:$0000–$FFFF` | ROM |
| `$00–$3E:$6000–$7FFF` (+$80 mirror) | Game Pak RAM |
| `$70–$71:$0000–$FFFF`, `$F0–$F1:$0000–$FFFF` | Game Pak RAM |

The internal MMIO mirror is `$3000 | addr.bit(0,9)` → the live window is `$3000–$33FF`
mirrored up to `$34FF` (mask `addr & 0x33FF`). luna should treat $3000–$34FF in
$00–$3F:/$80–$BF as the MMIO region.

### 3.3 Bus-conflict behaviour while the GSU runs

When the GSU is running (`sfr.g`) **and** holds ROM access (`scmr.ron`), SNES reads of
the ROM return a **fixed vector**, not ROM data:
```
vector[16] = {00,01,00,01,04,01,00,01,00,01,08,01,00,01,0c,01}; return vector[addr&15];
```
This is the canonical "GSU busy" reset/IRQ vector pattern the SNES sees so it doesn't
execute garbage. Equivalently: odd addresses → 0x01, even addresses →
{0,0,0x04,0,0,0,0x08,0,0x0C} per `addr&0x0E`, gated on running && ROM access.

When the GSU runs **and** holds RAM access (`scmr.ran`), SNES reads of Game Pak RAM
return **open bus** (the passed `data`). SNES writes still go through (the write lands).

BRAM (backup RAM) writes from the SNES are gated by `bramr`.

---

## 4. Pixel-plot pipeline

### 4.1 PixelCache structure

Two caches (`pixelcache[2]`):
```
struct PixelCache { n16 offset; n8 bitpend; n8 data[8]; }
```
`offset` = tile-row position `(y<<5)+(x>>3)` (8-pixel run), `bitpend` = per-x written-bit
mask (8 bits), `data[8]` = color index per x in the run. pixelcache[0] = primary (current
8-pixel run), pixelcache[1] = secondary (one-deep writeback queue). (An equivalent form
stores X(&0xF8)/Y instead of a packed offset, but the geometry is identical.)

### 4.2 color() — COLR/GETC color resolution

```
if(por.highnibble)  return (colr & 0xf0) | (source >> 4);
if(por.freezehigh)  return (colr & 0xf0) | (source & 0x0f);
return source;
```
Used by COLOR ($4E alt0) and GETC ($DF alt0).

### 4.3 plot()

1. **Transparency test** (skip plot if transparent & `!por.transparent`):
   - md==3 (8bpp): if `por.freezehigh` test `(colr&0x0f)==0`, else test `colr==0`.
   - else: test `(colr&0x0f)==0`.

   **Edge case worth noting:** the verified reference only special-cases md==3,
   testing the low *nibble* in all non-8bpp modes (so 2bpp tests bits 3:0, not 1:0).
   A bpp-exact reading would test exactly `bpp` low bits, which differs for 2bpp
   (md==0). **Follow the verified low-nibble behaviour** — flag as a candidate to
   revisit if a 2bpp-plot test ROM disagrees.
2. **Dither**: if `por.dither && md!=3`: if `(x^y)&1` use high nibble
   (`color>>=4`), then `color &= 0x0f`.
3. **Cache slot**: `offset=(y<<5)+(x>>3)`; if it differs from
   pixelcache[0].offset → flush pixelcache[1] to RAM, move [0]→[1], clear [0]
   (bitpend=0, offset=new).
4. **Write pixel**: `x=(x&7)^7` (bit-reverse within the run);
   `data[x]=color; bitpend |= 1<<x`; if bitpend==0xff (run full) flush [1], move [0]→[1],
   clear [0].

### 4.4 flushPixelCache() — writeback to Game Pak RAM

Converts the run's `data[8]` planar color indices into bitplane bytes and writes them to
the tile at `0x700000 + cn*(bpp<<3) + (scbr<<10) + (y&7)*2`. Character number `cn` from the
tile-index formula (§4.6). For each of `bpp` planes `n`:
- `byte = ((n>>1)<<4) + (n&1)` → byte offsets {0,1,16,17,32,33,48,49}.
- Build `data` by gathering bit n of each `cache.data[x]` into bit x.
- If `bitpend != 0xff` (partial run): read-modify-write — `step(clsr?5:6)`,
  `data &= bitpend`, `data |= read(addr+byte) & ~bitpend`.
- `step(clsr?5:6); write(addr+byte, data)`.
- Clear bitpend.

(The reference waits for RAM access before each plane write, folded into the RAM stall in
write().)

### 4.5 rpix() — pixel readback

Flush both caches first (secondary then primary), then read the `bpp`
bitplane bytes at the same tile address and reassemble the color index of pixel (x,y):
- `x=(x&7)^7`; for each plane n: `byte=((n>>1)<<4)+(n&1)`; `step(clsr?5:6)`;
  `data |= ((read(addr+byte)>>x)&1)<<n`.
RPIX sets s,z from the result and writes dr. (Whether the step happens before or after
the read is cosmetic — same cycle total.)

### 4.6 Tile-index (character number) formula

Selector: `por.obj ? 3 : scmr.ht`:
```
ht 0: cn = ((x&0xf8)<<1) + ((y&0xf8)>>3)
ht 1: cn = ((x&0xf8)<<1) + ((x&0xf8)>>1) + ((y&0xf8)>>3)
ht 2: cn = ((x&0xf8)<<1) + ((x&0xf8)<<0) + ((y&0xf8)>>3)
ht 3: cn = ((y&0x80)<<2) + ((x&0x80)<<1) + ((y&0x78)<<1) + ((x&0x78)>>3)   // OBJ/160-px
```
Tile byte address: `0x700000 + cn*(bpp<<3) + (scbr<<10) + (y&7)*2`.
(`ScreenBase`==scbr.)

`bpp = 2 << (md - (md>>1))` → md{0,1,2,3}→bpp{2,4,4,8}; `(bpp<<3)` = bytes
per tile (16/32/32/64). Bitplane interleave {0,1,16,17,32,33,48,49} is standard SNES
planar layout (planes 0/1 adjacent, planes 2/3 at +16, etc.).

---

## 5. Cache RAM (512-byte instruction cache)

Structure: `cache.buffer[512]`, `cache.valid[32]`. 32 lines × 16 bytes. `cbr` is the
cache base PC (bits 15:4).

**Fetch path** `readOpcode(address)`:
1. `offset = address - cbr`.
2. If `offset < 512` (in-cache window):
   - line = `offset>>4`. If `!valid[line]`: fill the 16-byte line — `dp = offset & 0xfff0`,
     `sp = (pbr<<16) + ((cbr+dp)&0xfff0)`; loop 16: `step(clsr?5:6); buffer[dp++]=read(sp++)`;
     then `valid[line]=true`.
   - else (hit): `step(clsr?1:2)`.
   - return `buffer[offset]`.
3. Else (outside cache window): if `pbr<=0x5f` syncROMBuffer else syncRAMBuffer; then
   `step(clsr?5:6); return read((pbr<<16)|address)`.

(A bulk implementation fills the line then does a single `Step(clsr?5*16:6*16)` — one
step of 16× the per-byte cost — for the same total. Cache-hit cost `Step(clsr?1:2)`.)

**Cache addressing for MMIO ($3100–$32FF):** `(address + cbr) & 511`.
Writing the 16th byte of a line (`(address&15)==15`) marks that line valid —
this is how the SNES can preload the cache.

**Invalidation / flush** `flushCache()` = set all 32 valid flags false.
Triggered by:
- CACHE op when cbr changes.
- LJMP.
- MMIO write to PBR $3034.
- g 1→0 transition on $3030 write (also zeroes cbr).

---

## 6. Timing model

### 6.1 Clock select & step granularity

`clsr` selects fast (21.4 MHz, clsr=1) vs slow (10.7 MHz, clsr=0). Throughout, the per-event
cost is `clsr ? fast : slow`. The two base values are **F=5 / S=6** for memory cycles and
**F=1 / S=2** for cache-hit fetch cycles. `step(clocks)`:
1. Service pending ROM buffer: `romcl -= min(clocks,romcl)`; when it hits 0, `sfr.r=0`,
   `romdr = read((rombr<<16)+r14)`.
2. Service pending RAM write: `ramcl -= min(clocks,ramcl)`; when 0, `write(0x700000 +
   (rambr<<16) + ramar, ramdr)`.
3. Advance the coprocessor clock by `clocks` and re-synchronise to the SNES.

(Equivalently: accumulate the cycle count, decrement RomDelay/RamDelay, fire the buffered
ROM read / RAM write on reaching 0.)

### 6.2 ROM buffer (R14-triggered)

- `updateROMBuffer()`: `sfr.r=1; romcl = clsr?5:6`. Armed when R14 is written.
- `syncROMBuffer()`: if `romcl` pending, `step(romcl)` to force completion.
- `readROMBuffer()`: syncROMBuffer then return `romdr`.
Used by GETB/GETC/ROMB.

### 6.3 RAM buffer (delayed write)

- `writeRAMBuffer(addr,data)`: syncRAMBuffer first (drain prior pending write), then
  `ramcl = clsr?5:6; ramar=addr; ramdr=data`. The actual store lands
  `ramcl` cycles later in step().
- `readRAMBuffer(addr)`: syncRAMBuffer then `read(0x700000 + (rambr<<16) + addr)`.
  **Reads are not buffered** — only writes are delayed.
- `syncRAMBuffer()`: `if(ramcl) step(ramcl)`.

### 6.4 Per-instruction explicit cycle costs (step() calls inside ops)

Most ops cost only their fetch (cache hit F1/S2, or fill/non-cache F5/S6). Ops that add
explicit `step()`:

| Op | extra step |
|---|---|
| MULT/UMULT | if `!ms0`: `clsr?1:2` |
| FMULT/LMULT | `(ms0?3:7) * (clsr?1:2)` |
| RPIX | per plane (bpp): `clsr?5:6` |
| PLOT cache flush | per plane, +RMW when partial: each `clsr?5:6` |
| cache line fill | 16 × `clsr?5:6` |
| non-cache fetch | `clsr?5:6` |
| cache hit fetch | `clsr?1:2` |
| ROM/RAM access stall | `6` per spin while access denied |

### 6.5 Lockstep with the SNES CPU

The SuperFX runs as a cooperative thread; every `step()` ends by re-synchronising to the
SNES CPU, and the GSU's main loop runs one instruction per dispatch, idling `step(6)` per call when
`sfr.g==0`. Any SNES MMIO access calls `cpu.synchronize(*this)` first to align clocks
before touching state. A catch-up driver is equivalent: execute until
`CycleCount >= masterClock*clockMultiplier`, then a final Step to align. luna's existing
coproc scheduler should drive the GSU like SA-1 — run N GSU cycles per SNES master-clock
budget, with a synchronize on every MMIO touch.

The GSU thread base clock is fixed; the clsr fast/slow ratio is expressed through the
per-event step counts, not by changing the base frequency. (Any clock-multiplier knob is
an overclock UI feature, not hardware — ignore for accuracy.)

---

## 7. Control flow / handshake

### 7.1 Start (GO)

The SNES launches the GSU by writing R15 (the PC) via MMIO, **high byte last**: writing
$301F (R15 high byte) sets `sfr.g=1` as a side effect. So the canonical launch is: set up
registers, write R15 low ($301E) then R15 high ($301F) → GO. Alternatively the SNES can
set sfr.g via the $3030 SFR write, but the documented trigger is the R15-high write. (A
write to register 14 separately arms the ROM buffer.)

On launch the GSU begins fetching from `(pbr<<16)|r15` through the cache.
`pipeline` starts as 0x01 (NOP) so the first executed instruction is a NOP —
gives the pipeline one slot to prime.

### 7.2 Stop / HALT

- STOP op ($00): `if(!cfgr.irq){ sfr.irq=1; stop(); }` then `sfr.g=0; pipeline=NOP; reset()`.
  `stop()` → `cpu.irq(1)` raises the SNES IRQ.
- The SNES can also clear g by writing $3030 with bit5=0; the g 1→0 transition resets
  `cbr=0` and flushes the cache.

While `sfr.g==0`, `main()` just idles `step(6)` and does not fetch/execute.

### 7.3 IRQ generation & acknowledge

- STOP raises IRQ unless masked by `cfgr.irq` (the "IRQ disable" bit): only when
  `cfgr.irq==0` does STOP set `sfr.irq=1` and call `stop()`→`cpu.irq(1)`. (Note the
  inverted sense: `cfgr.irq==1` *masks* the interrupt.)
- The SNES acknowledges by **reading SFR high byte $3031**, which clears `sfr.irq` and
  deasserts the line: `regs.sfr.irq=0; cpu.irq(0)`.

### 7.4 Reset state

See §1.8. Key: vcr=0x04, pipeline=0x01 (NOP), sfr.g=0 (halted), all banks 0, cache invalid,
pixelcache offsets ~0.
