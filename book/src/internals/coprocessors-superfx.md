# SuperFX / GSU (Graphics Support Unit) — Implementation Reference Spec

Diff target for the luna Rust port. Every claim is cited to a line in the fetched
references:

- ares GSU core: `/tmp/ares/gsu_core/{gsu.cpp,gsu.hpp,instruction.cpp,instructions.cpp,registers.hpp,disassembler.cpp}`
- ares SFC SuperFX wrapper: `/tmp/ares/superfx/{superfx.hpp,superfx.cpp,core.cpp,io.cpp,memory.cpp,bus.cpp,timing.cpp}`
- Mesen2 GSU: `/tmp/mesen2/gsu/{Gsu.h,Gsu.cpp,Gsu.Instructions.cpp,GsuTypes.h,GsuRomHandler.h,GsuRamHandler.h}`

Citations look like `(ares core.cpp:42)` / `(mesen Gsu.cpp:90)`. ares is the
authority unless explicitly noted; Mesen2 is the cross-check.

The GSU core (`struct GSU`, ares gsu.hpp) is the abstract CPU; the SoC wrapper
(`struct SuperFX : GSU, Thread`, ares superfx.hpp:1) supplies the virtuals
(`step/read/write/plot/rpix/pipe/...`). luna should mirror this split: a pure GSU
instruction engine + a SuperFX glue layer that owns the memory map, timing, MMIO,
and SNES handshake.

---

## 1. Register model

### 1.1 General-purpose registers R0–R15

16 × 16-bit registers (ares registers.hpp:121 `Register r[16]`; mesen GsuTypes.h:57
`uint16_t R[16]`). Each ares `Register` tracks a `modified` flag (registers.hpp:3)
set on every write (registers.hpp:9-12); the wrapper uses it to detect writes to
R14 and R15 (ares superfx.cpp:35-44).

Special-purpose roles:

| Reg | Role | Cite |
|---|---|---|
| R0  | default source & dest after `reset()` (`sreg=dreg=0`) | ares registers.hpp:153-154 |
| R1  | PLOT/RPIX X coordinate | ares instructions.cpp:129,132 |
| R2  | PLOT/RPIX Y coordinate | ares instructions.cpp:129,132 |
| R3  | (general) | — |
| R4  | LMULT low-16 result destination | ares instructions.cpp:298 |
| R6  | FMULT/LMULT implicit multiplicand | ares instructions.cpp:297 |
| R7  | MERGE high source (`r7 & 0xff00`) | ares instructions.cpp:198 |
| R8  | MERGE low source (`r8 >> 8`) | ares instructions.cpp:198 |
| R11 | LINK destination (return address) | ares instructions.cpp:240 |
| R12 | LOOP counter (decremented) | ares instructions.cpp:90 |
| R13 | LOOP branch target | ares instructions.cpp:93 |
| R14 | **ROM-load trigger**: writing R14 starts a ROM buffer fetch | ares superfx.cpp:35-38, io.cpp:68, timing.cpp:30-33 |
| R15 | **Program counter** | ares memory.cpp:75,82; superfx.cpp:40-44 |

**R14 ROM-load trigger.** After any instruction, if `regs.r[14].modified` the
wrapper calls `updateROMBuffer()` (ares superfx.cpp:35-38). Also on a direct MMIO
write to R14 from the SNES (ares io.cpp:68). `updateROMBuffer()` sets `sfr.r=1` and
arms `romcl` (timing.cpp:30-33). The fetch at `(rombr<<16)+r14` lands `romcl` cycles
later (timing.cpp:5-8). Mesen mirrors: WriteRegister reg==14 → `SFR.RomReadPending=true`,
`RomDelay = ClockSelect?5:6` (mesen Gsu.cpp:346-348).

**R15 PC autoincrement.** After executing an instruction, if R15 was *not* written
by it, R15 is incremented (ares superfx.cpp:40-44; mesen Gsu.cpp:236-240). Branch /
JMP / LOOP write R15 explicitly to suppress the increment.

### 1.2 SFR — Status Flag Register (16-bit, MMIO $3030/$3031)

Bit layout (ares registers.hpp:36-51):

| Bit | Name | Meaning | Cite |
|---|---|---|---|
| 0  | — | (always 0) | |
| 1  | z   | zero flag | registers.hpp:38 |
| 2  | cy  | carry flag | registers.hpp:39 |
| 3  | s   | sign flag | registers.hpp:40 |
| 4  | ov  | overflow flag | registers.hpp:41 |
| 5  | g   | **go** flag (GSU running) | registers.hpp:42 |
| 6  | r   | ROM-read pending (R14 fetch in flight) | registers.hpp:43 |
| 7  | — | (always 0) | |
| 8  | alt1 | ALT1 prefix mode active | registers.hpp:44 |
| 9  | alt2 | ALT2 prefix mode active | registers.hpp:45 |
| 10 | il  | immediate-lower latch | registers.hpp:46 |
| 11 | ih  | immediate-upper latch | registers.hpp:47 |
| 12 | b   | "with" (B) prefix flag | registers.hpp:48 |
| 13–14 | — | (always 0) | |
| 15 | irq | interrupt asserted to SNES | registers.hpp:49 |

Composite `alt` = bits 8:9 (`BitRange<16,8,9>`, registers.hpp:51) — the 2-bit
ALT mode selector {alt0,alt1,alt2,alt3}.

**Read mask:** SFR reads back as `data & 0x9f7e` (ares registers.hpp:57) — i.e.
bits {1,2,3,4,5,6, 8,9,10,11,12, 15} are visible; bits 0,7,13,14 read 0.
Mesen builds the byte explicitly: low byte = z<<1|cy<<2|s<<3|ov<<4|g<<5|r<<6
(mesen GsuTypes.h:20-30); high byte = alt1<<0|alt2<<1|il<<2|ih<<3|b<<4|irq<<7
(GsuTypes.h:32-42). Note Mesen low byte omits bit 0 and bit 7, matching ares 0x7e
low / 0x9f high.

### 1.3 PBR / ROMBR / RAMBR / CBR / SCBR

| Reg | Width | Role | Reset | Cite |
|---|---|---|---|---|
| pbr   | n8   | Program Bank Register — bank for PC fetches; ≤$5F = ROM, ≥$60 = RAM | 0x00 | registers.hpp:124; gsu.cpp:22; fetch memory.cpp:60-70 |
| rombr | n8   | ROM data Bank Register — bank for R14 ROM buffer fetch / GETx | 0x00 | registers.hpp:125; timing.cpp:7 |
| rambr | bool | RAM data Bank Register — 1-bit RAM bank for LD/ST buffer ops | 0 | registers.hpp:126; timing.cpp:13,41 |
| cbr   | n16  | Cache Base Register — base PC of the 512-byte instruction cache window; only bits 15:4 meaningful (`&0xfff0`) | 0x0000 | registers.hpp:127; CACHE instructions.cpp:19-21 |
| scbr  | n8   | Screen Base Register — Game Pak RAM tile-map base, used as `scbr<<10` | 0x00 | registers.hpp:128; core.cpp:60,87 |
| colr  | n8   | Color Register (PLOT color) | 0x00 | registers.hpp:129; core.cpp:24 |

`pbr` writable range is masked to 7 bits on MMIO write (`data & 0x7f`, ares
io.cpp:93) and on LJMP (`r[n] & 0x7f`, instructions.cpp:278). `rombr` masked 7-bit
(instructions.cpp:378). `rambr` masked 1-bit (instructions.cpp:375).

### 1.4 SCMR — Screen Mode Register ($303A) — **scrambled bit layout**

Fields (ares registers.hpp:61-79):
- `ht` (height select, **2 bits split across two non-adjacent SCMR bits**)
- `ron` (1 = GSU has ROM access; SNES locked out)
- `ran` (1 = GSU has RAM access; SNES locked out)
- `md`  (color depth mode, 2 bits)

The wire byte ↔ field mapping is deliberately scrambled. **Write** `scmr = data`
(registers.hpp:71-78):
```
ht  = ((data & 0x20) ? 1 : 0) << 1   // ht bit1 <- byte bit5
ht |= ((data & 0x04) ? 1 : 0) << 0   // ht bit0 <- byte bit2
ron = data & 0x10                    // byte bit4
ran = data & 0x08                    // byte bit3
md  = data & 0x03                    // byte bits1:0
```
**Read** `operator u32` (registers.hpp:67-69) reverses it:
```
byte = ((ht>>1)<<5) | (ron<<4) | (ran<<3) | ((ht&1)<<2) | md
```
Byte bit map: `bit5=ht_hi, bit4=ron, bit3=ran, bit2=ht_lo, bits1:0=md`.

Mesen cross-check (mesen Gsu.cpp:562-581): on $303A write,
`ColorGradient=value&0x03`; `ScreenHeight = ((value&0x04)>>2) | ((value&0x20)>>4)`
(== ht_lo from bit2, ht_hi from bit5 — identical to ares); `GsuRamAccess=value&0x08`
(==ran); `GsuRomAccess=value&0x10` (==ron). **Agrees with ares.**

`md` → bpp mapping (both refs): md∈{0,1,2,3} → bpp∈{2,4,4,8}. ares computes
`bpp = 2 << (md - (md>>1))` (core.cpp:59,86). Mesen uses an explicit switch
(Gsu.cpp:564-569). md==3 (8bpp) is the "OBJ-capable" / transparency-special mode
(core.cpp:13-21).

### 1.5 POR — Plot Option Register (set by CMODE, $4E alt1)

Fields (ares registers.hpp:81-100), set from source register low byte:

| Bit | Field | Meaning | Cite |
|---|---|---|---|
| 0 | transparent | 1 = plot even transparent (color 0) pixels | registers.hpp:97; core.cpp:12 |
| 1 | dither | 1 = dither color by (x^y)&1 (non-8bpp only) | registers.hpp:96; core.cpp:25-28 |
| 2 | highnibble | color() returns `(colr&0xf0)|(src>>4)` | registers.hpp:95; core.cpp:6 |
| 3 | freezehigh | color() returns `(colr&0xf0)|(src&0x0f)`; also gates md==3 transparency on low nibble | registers.hpp:94; core.cpp:7,14-16 |
| 4 | obj  | OBJ mode — forces tile-index calc to ht==3 layout | registers.hpp:93; core.cpp:53,80 |

Mesen names: PlotTransparent/PlotDither/ColorHighNibble/ColorFreezeHigh/ObjMode,
same bit order (mesen Gsu.Instructions.cpp:597-601). **Agrees.**

### 1.6 CFGR — Config Register ($3037)

Fields (ares registers.hpp:102-115):
- `irq` (bit 7): 1 = **mask** GSU IRQ (STOP will not raise IRQ) — see io.cpp note
- `ms0` (bit 5): 1 = high-speed multiply (MULT/FMULT timing)

`cfgr = data` → `irq=data&0x80; ms0=data&0x20` (registers.hpp:110-113).
Mesen: `HighSpeedMode=value&0x20; IrqDisabled=value&0x80` (mesen Gsu.cpp:554-557).
**Agrees** (Mesen's `IrqDisabled` == ares `cfgr.irq`).

### 1.7 Other single-bit / misc registers

- `bramr` (bool) — Backup-RAM write enable ($3033, `data&0x01`); when 0, SNES
  writes to BRAM are dropped (ares io.cpp:88-90; bus.cpp:60-62). Mesen
  `BackupRamEnabled` (Gsu.cpp:551).
- `vcr` (n8) — Version Code Register, read-only at $303B, reset 0x04
  (ares registers.hpp:132; gsu.cpp:31; io.cpp:33-35). Mesen returns hardcoded 0x04
  (Gsu.cpp:490, comment "can be 1 or 4?").
- `clsr` (bool) — Clock Select Register ($3039, `data&0x01`): 0 = 10.7 MHz,
  1 = 21.4 MHz. Affects every cycle cost (`clsr ? fast : slow`). (ares registers.hpp:134;
  io.cpp:105-107). Mesen `ClockSelect` (Gsu.cpp:560).
- `pipeline` (n8) — the prefetched opcode (the 1-stage pipe). Reset to 0x01 (NOP)
  on power (ares gsu.cpp:34; mesen ProgramReadBuffer=0x01, Gsu.cpp:29/453).
- `ramaddr` (n16) — last RAM address used by LD/ST (for SBK) (registers.hpp:119).

### 1.8 Reset / power state (ares gsu.cpp:15-37, superfx.cpp:58-84)

All R[]=0, sfr=0, pbr=rombr=0, rambr=0, cbr=0, scbr=0, scmr=0, colr=0, por=0,
bramr=0, **vcr=0x04**, cfgr=0, clsr=0, pipeline=0x01 (NOP), ramaddr=0.
`reset()` also zeroes sfr.b/alt1/alt2 and sreg=dreg=0 (registers.hpp:148-155).
Wrapper power: cache.buffer all 0, all 32 valid flags false, pixelcache[0/1].offset=~0,
bitpend=0, romcl=romdr=ramcl=ramar=ramdr=0 (superfx.cpp:71-83).

### 1.9 MMIO address map ($3000–$303F, cache $3100–$32FF)

From ares io.cpp. SNES sees these at `$00-$3F:$3000-$34FF` (ares mapping; see §3).
Internally `address = 0x3000 | addr.bit(0,9)` (io.cpp:3) — mirrors every 0x400.

| Addr | R/W | Function | Cite |
|---|---|---|---|
| $3000–$301F | R/W | R0–R15, little-endian byte pairs: reg = `addr>>1 & 15`, byte = `addr&1` | io.cpp:9-11, 61-72 |
| $301F (W high byte of R15) | W | **also sets sfr.g=1 (GO)** — writing R15 high byte launches the GSU | io.cpp:70 |
| $3030 | R | SFR low byte | io.cpp:15 |
| $3030 | W | SFR low byte; if g 1→0, cbr=0 + flushCache | io.cpp:75-82 |
| $3031 | R | SFR high byte; **side effect: clears sfr.irq and deasserts CPU IRQ** | io.cpp:18-22 |
| $3031 | W | SFR high byte | io.cpp:84-86 |
| $3033 | W | BRAMR (`data&0x01`) | io.cpp:88-90 |
| $3034 | R | PBR | io.cpp:25-27 |
| $3034 | W | PBR (`data&0x7f`) + flushCache | io.cpp:92-95 |
| $3036 | R | ROMBR | io.cpp:29-31 |
| $3037 | W | CFGR | io.cpp:97-99 |
| $3038 | W | SCBR | io.cpp:101-103 |
| $3039 | W | CLSR (`data&0x01`) | io.cpp:105-107 |
| $303A | W | SCMR (scrambled, §1.4) | io.cpp:109-111 |
| $303B | R | VCR (0x04) | io.cpp:33-35 |
| $303C | R | RAMBR | io.cpp:37-39 |
| $303E | R | CBR low byte | io.cpp:41-43 |
| $303F | R | CBR high byte | io.cpp:45-47 |
| $3100–$32FF | R/W | instruction cache RAM (512B), indexed `(addr-0x3100+cbr)&511` | io.cpp:5-7,57-59; memory.cpp:91-100 |
| (other) | R | 0x00 | io.cpp:50 |

Every MMIO access first calls `cpu.synchronize(*this)` (io.cpp:2,54) to catch the
coprocessor up to the SNES clock before the access.

**Mesen access-gating divergence:** Mesen blocks most MMIO while the GSU is running
— `Read` returns 0 for all but $3030/$3031/$303B when `SFR.Running` (mesen
Gsu.cpp:466-469); `Write` allows only $3030/$303A while running (Gsu.cpp:509-512),
citing "During GSU operation, only SFR, SCMR, and VCR may be accessed." **ares does
NOT implement this lockout** (io.cpp serves all addresses unconditionally). ares is
the authority for luna; treat the Mesen lockout as a hardware nicety, not required
for correctness of the games tested. Document it but follow ares.

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

(ares disassembler.cpp:22-27 selects ALT0/1/2/3 from `alt2<<1|alt1`.)

- `$3D ALT1`: `sfr.b=0; sfr.alt1=1` (ares instructions.cpp:98-101)
- `$3E ALT2`: `sfr.b=0; sfr.alt2=1` (instructions.cpp:104-107)
- `$3F ALT3`: `sfr.b=0; sfr.alt1=1; sfr.alt2=1` (instructions.cpp:110-114)

ALT1/2/3 are themselves prefix instructions: they do **not** call `reset()`, so the
alt bits survive into the *next* instruction. Every *non-prefix* instruction ends
with `regs.reset()` which clears b, alt1, alt2 and resets sreg/dreg to 0
(registers.hpp:148-155). Thus alt flags are consumed by exactly one following op.
Mesen `ResetFlags()` is identical (mesen Gsu.cpp:354-362, called at end of each op).

The prefix ops TO/FROM/WITH set sreg/dreg and the `b` ("with") flag (§2.2) and also
**do not** call reset() in their prefix role.

### 2.2 Register-select prefixes: TO / FROM / WITH and the B flag

These build the source/dest operand selection for the *next* ALU/move op:

- `$B0–$BF FROM rN` (ALT0, b=0): `sreg = n` (set source). (ares instructions.cpp:327-329)
- `$10–$1F TO rN` (ALT0, b=0): `dreg = n` (set dest). (instructions.cpp:63-65)
- `$20–$2F WITH rN`: `sreg = n; dreg = n; sfr.b = 1`. (instructions.cpp:73-77)

The `b` ("with") flag changes TO/FROM into the *move* variants:

- `$10–$1F MOVE rN` (b=1): `r[n] = sr()` then reset(). (instructions.cpp:66-69)
- `$B0–$BF MOVES rN` (b=1): `dr() = r[n]`, sets ov/s/z, then reset().
  (instructions.cpp:330-336)

So: a bare `FROM rA` / `TO rB` only *latches* sreg/dreg and the next op uses them
(this is how `ADD rN` reads `sr()` as src and writes `dr()` as dest). A `WITH rA`
sets both and the `b` flag, making the immediately following `TO`/`FROM` act as
register-to-register MOVE/MOVES. After any non-prefix op, reset() restores sreg=dreg=0
(so the implicit operand defaults back to R0).

`sr()` = `r[sreg]`, `dr()` = `r[dreg]` (ares registers.hpp:145-146).

### 2.3 The pipeline / operand fetch

`pipe()` returns the prefetched byte `regs.pipeline` and prefetches the next opcode
from `++r15` (ares memory.cpp:80-85). `peekpipe()` returns pipeline but prefetches
from the *current* r15 without incrementing the returned value's basis
(memory.cpp:73-78); the main loop uses `peekpipe()` to get the opcode (superfx.cpp:31).
Operand bytes (branch displacement, immediates, RAM short/long addresses) are read
via `pipe()` (e.g. instructions.cpp:57, 312, 410-411, 420-421).

Mesen splits this into `ReadOpCode()` (mesen Gsu.cpp:300-305) and `ReadOperand()`
(Gsu.cpp:292-298); ReadOperand increments R15 then refills ProgramReadBuffer.

### 2.4 Opcode table

Notation: `sr` = `r[sreg]`, `dr` = `r[dreg]`, `n` = low nibble of opcode (register
index) or immediate as noted. "reset" = clears alt/b + sreg/dreg unless it's a
prefix op. Flag column lists flags written. Cycle cost is the *extra* cost beyond
the base fetch/decode; base instruction execution is implicitly the pipeline step
(see §6) — only ops that call `step()` themselves add the listed extra cycles.
`F` = fast-clock cycles (clsr=1), `S` = slow (clsr=0); where a single number is
shown the op adds no explicit step().

Dispatch table is ares instruction.cpp:35-86. Effects from instructions.cpp;
Mesen cross-cites to Gsu.Instructions.cpp.

| Opcode | ALT0 (default) | ALT1 | ALT2 | ALT3 | Effect / notes | Cite |
|---|---|---|---|---|---|---|
| $00 | stop | stop | stop | stop | if `!cfgr.irq`: sfr.irq=1, raise CPU IRQ. sfr.g=0; pipeline=NOP; reset() | instr.cpp:2-10 |
| $01 | nop | nop | nop | nop | reset() only | instr.cpp:13-15 |
| $02 | cache | cache | cache | cache | if cbr≠(r15&0xfff0): cbr=r15&0xfff0, flushCache | instr.cpp:18-24 |
| $03 | lsr | lsr | lsr | lsr | cy=sr&1; dr=sr>>1; s,z | instr.cpp:27-33 |
| $04 | rol | rol | rol | rol | dr=(sr<<1)|cy_in; cy=sr&0x8000; s,z | instr.cpp:36-43 |
| $05 | bra e | (same all modes) | | | unconditional; disp=`(i8)pipe()`; if take r15+=disp | instr.cpp:56-59 |
| $06 | b** (see note) | | | | cond branch — `(s^ov)==0` | instr.cpp:42 (dispatch) |
| $07 | b** | | | | cond branch — `(s^ov)==1` | instr.cpp:43 |
| $08 | bne | | | | `z==0` | instr.cpp:44 |
| $09 | beq | | | | `z==1` | instr.cpp:45 |
| $0A | bpl | | | | `s==0` | instr.cpp:46 |
| $0B | bmi | | | | `s==1` | instr.cpp:47 |
| $0C | bcc | | | | `cy==0` | instr.cpp:48 |
| $0D | bcs | | | | `cy==1` | instr.cpp:49 |
| $0E | bvc | | | | `ov==0` | instr.cpp:50 |
| $0F | bvs | | | | `ov==1` | instr.cpp:51 |
| $10–$1F | to rN / move rN(b=1) | (same) | | | b=0: dreg=N; b=1: r[N]=sr, reset | instr.cpp:63-70 |
| $20–$2F | with rN | (same) | | | sreg=dreg=N; sfr.b=1 (prefix, no reset) | instr.cpp:73-77 |
| $30–$3B | stw (rN) | stb (rN) | stw | stb | ramaddr=r[N]; writeRAMBuffer(addr, sr); if !alt1 also (addr^1, sr>>8) | instr.cpp:81-86 |
| $3C | loop | loop | loop | loop | r12--; s,z from r12; if !z r15=r13 | instr.cpp:89-95 |
| $3D | alt1 | — | — | — | b=0; alt1=1 (prefix) | instr.cpp:98-101 |
| $3E | alt2 | — | — | — | b=0; alt2=1 (prefix) | instr.cpp:104-107 |
| $3F | alt3 | — | — | — | b=0; alt1=alt2=1 (prefix) | instr.cpp:110-114 |
| $40–$4B | ldw (rN) | ldb (rN) | ldw | ldb | ramaddr=r[N]; dr=readRAMBuffer(addr); if !alt1 dr|=readRAMBuffer(addr^1)<<8 | instr.cpp:118-123 |
| $4C | plot | rpix | plot | rpix | see §4. RPIX adds bpp×(F5/S6) step | instr.cpp:127-137 |
| $4D | swap | swap | swap | swap | dr=(sr>>8)|(sr<<8); s,z | instr.cpp:140-145 |
| $4E | color | cmode | color | cmode | alt0: colr=color(sr); alt1: por=sr (CMODE) | instr.cpp:149-156 |
| $4F | not | not | not | not | dr=~sr; s,z | instr.cpp:159-164 |
| $50–$5F | add rN | adc rN | add #N | adc #N | r=sr+op(+cy if alt1); ov,s,cy,z; dr=r | instr.cpp:170-179 |
| $60–$6F | sub rN | sbc rN | sub #N | cmp rN | r=sr-op(-!cy if sbc); ov,s,cy,z; dr=r (CMP: no write) | instr.cpp:185-194 |
| $70 | merge | merge | merge | merge | dr=(r7&0xff00)|(r8>>8); ov,s,cy,z special masks | instr.cpp:197-204 |
| $71–$7F | and rN | bic rN | and #N | bic #N | dr=sr & (alt1?~op:op); s,z | instr.cpp:210-216 |
| $80–$8F | mult rN | umult rN | mult #N | umult #N | signed/unsigned 8×8→16; s,z; if !ms0 add F1/S2 step | instr.cpp:222-229 |
| $90 | sbk | sbk | sbk | sbk | writeRAMBuffer(ramaddr,sr); (ramaddr^1, sr>>8) | instr.cpp:232-236 |
| $91–$94 | link #N | (same) | | | r11=r15+N (N=1..4 = low nibble) | instr.cpp:239-242 |
| $95 | sex | sex | sex | sex | dr=(i8)sr (sign-extend low byte); s,z | instr.cpp:245-250 |
| $96 | asr | div2 | asr | div2 | cy=sr&1; dr=(i16)sr>>1 (+rounding if div2); s,z | instr.cpp:254-260 |
| $97 | ror | ror | ror | ror | dr=(cy<<15)|(sr>>1); cy=sr&1; s,z | instr.cpp:263-270 |
| $98–$9D | jmp rN | ljmp rN | jmp | ljmp | alt0: r15=r[N]; alt1: pbr=r[N]&0x7f, r15=sr, cbr=r15&0xfff0, flushCache | instr.cpp:274-284 |
| $9E | lob | lob | lob | lob | dr=sr&0xff; s(bit7),z | instr.cpp:287-292 |
| $9F | fmult | lmult | fmult | lmult | (i16)sr*(i16)r6; dr=res>>16; cy=res&0x8000; if lmult r4=res; s,z; step (ms0?3:7)×(clsr?1:2) | instr.cpp:296-305 |
| $A0–$AF | ibt rN,#pp | lms rN,(yy) | sms (yy),rN | (=alt1 lms) | alt1: short-addr load; alt2: short-addr store; else r[N]=(i8)pipe() | instr.cpp:310-323 |
| $B0–$BF | from rN / moves(b=1) | (same) | | | b=0: sreg=N; b=1: dr=r[N], ov/s/z, reset | instr.cpp:327-337 |
| $C0 | hib | hib | hib | hib | dr=sr>>8; s(bit7),z | instr.cpp:340-345 |
| $C1–$CF | or rN | xor rN | or #N | xor #N | dr=sr|op or sr^op; s,z | instr.cpp:351-357 |
| $D0–$DE | inc rN | (same) | | | r[N]++; s,z | instr.cpp:360-365 |
| $DF | getc | (getc) | ramb | romb | alt0: colr=color(readROMBuffer); alt2&!alt1: syncRAM, rambr=sr&1; alt3: syncROM, rombr=sr&0x7f | instr.cpp:370-381 |
| $E0–$EE | dec rN | (same) | | | r[N]--; s,z | instr.cpp:384-389 |
| $EF | getb | getbh | getbl | getbs | ROM buffer byte → dr per alt2:alt1 (see below) | instr.cpp:395-403 |
| $F0–$FF | iwt rN,#xx | lm rN,(xx) | sm (xx),rN | (=alt1 lm) | alt1: long-addr load; alt2: long-addr store; else r[N]=word immediate | instr.cpp:408-424 |

**Branch-name $06/$07 caution.** ares dispatch maps `$06 → Branch((s^ov)==0)` and
`$07 → Branch((s^ov)==1)`, and the ares *disassembler* labels $06="blt", $07="bge"
(disassembler.cpp:46-47). Mesen maps `$06 → BGE` (`Sign==Overflow`) and `$07 → BLT`
(`Sign!=Overflow`) (mesen Gsu.cpp:113-114, Gsu.Instructions.cpp:46-53). **These are
the same branch *conditions* — only the mnemonics are swapped between the two
references.** For luna only the *condition per opcode byte* matters, and both agree:
`$06 → take when (s^ov)==0`, `$07 → take when (s^ov)==1`. Implement by condition,
ignore the mnemonic dispute.

**GETB ($EF) sub-decode** (`alt2<<1|alt1`, ares instr.cpp:396-401):
- 0 getb : `dr = romBuffer`
- 1 getbh: `dr = romBuffer<<8 | (n8)sr`
- 2 getbl: `dr = (sr&0xff00) | romBuffer`
- 3 getbs: `dr = (i8)romBuffer`

Mesen names map identically (mesen Gsu.Instructions.cpp:558-573) but note Mesen's
order: alt2&&alt1→GETBS, alt2→GETBL, alt1→GETBH, else GETB. Same truth table.

### 2.5 ALU flag details (exact)

- **ADD/ADC** (instr.cpp:170-179): operand `n = (alt2 ? immediate-nibble : r[n])`;
  carry-in = `alt1 ? cy : 0`. `r = sr + n + carryin`.
  `ov = ~(sr^n) & (n^r) & 0x8000`; `s = r&0x8000`; `cy = r>=0x10000`; `z = (u16)r==0`.
  Mesen identical (Gsu.Instructions.cpp:220-243).
- **SUB/SBC/CMP** (instr.cpp:185-194): operand selection `n = (!alt2 || alt1) ? r[n]
  : immediate` — i.e. immediate only for SUB#N (alt2 && !alt1). Borrow term:
  `(!alt2 && alt1) ? !cy : 0` (SBC). `r = sr - n - borrow`.
  `ov = (sr^n) & (sr^r) & 0x8000`; `s = r&0x8000`; `cy = r>=0`; `z=(u16)r==0`.
  Write dr **unless** CMP (alt3 = alt2&&alt1): `if(!alt2 || !alt1) dr=r`.
  Mesen identical (Gsu.Instructions.cpp:245-271); note Mesen's SBC borrow
  `result -= Carry ? 0 : 1` == ares `!cy`.
- **AND/BIC** (instr.cpp:210-216): `n = alt2 ? imm : r[n]`; `dr = sr & (alt1 ? ~n : n)`.
- **OR/XOR** (instr.cpp:351-357): `n = alt2 ? imm : r[n]`; `dr = alt1 ? sr^n : sr|n`.
- **MERGE** (instr.cpp:197-204): `dr=(r7&0xff00)|(r8>>8)`; flags use packed masks:
  `ov=dr&0xc0c0; s=dr&0x8080; cy=dr&0xe0e0; z=dr&0xf0f0` (note: z is *set* when any
  of those bits set — these are unusual "any-bit" flags, not standard z/s). **Both
  refs agree** (mesen Gsu.Instructions.cpp:204-207) though Mesen orders the
  assignments differently (cy,ov,s,z) — same masks.
- **ASR/DIV2** (instr.cpp:254-260): `cy=sr&1`; `dr = (i16)sr>>1 + (alt1 ? ((sr+1)>>16)
  : 0)`. The `(sr+1)>>16` rounding term is the DIV2 correction. Mesen identical
  (Gsu.Instructions.cpp:404-418).
- **MULT/UMULT** (instr.cpp:222-229): `n = alt2 ? imm : r[n]`;
  `dr = alt1 ? (u16)((u8)sr*(u8)n) : (u16)((i8)sr*(i8)n)` (8×8→16, low byte operands).
  Timing: if `!ms0` add `step(clsr?1:2)` (instr.cpp:228).
- **FMULT/LMULT** (instr.cpp:296-305): `result = (i16)sr * (i16)r6` (16×16→32);
  `dr = result>>16`; `cy = result&0x8000`; if LMULT `r4 = result` (low 16).
  Timing: `step((ms0?3:7) * (clsr?1:2))`. Mesen identical (Gsu.Instructions.cpp:301-319).

### 2.6 LD / ST and RAM-buffer ops

- **STW/STB ($30, store)**: `ramaddr=r[N]; writeRAMBuffer(ramaddr, sr)`; STW also
  `writeRAMBuffer(ramaddr^1, sr>>8)` (instr.cpp:81-86). writeRAMBuffer arms a delayed
  write (see §6).
- **LDW/LDB ($40, load)**: `ramaddr=r[N]; dr=readRAMBuffer(ramaddr)`; LDW also
  `dr |= readRAMBuffer(ramaddr^1)<<8` (instr.cpp:118-123).
- **SBK ($90)**: re-store word to last `ramaddr` (instr.cpp:232-236).
- **IBT/LMS/SMS ($A0)**: alt1 LMS load word from `(pipe()<<1)`; alt2 SMS store word to
  `(pipe()<<1)`; default IBT `r[N]=(i8)pipe()` (instr.cpp:310-323).
- **IWT/LM/SM ($F0)**: alt1 LM load word from 16-bit immediate addr (two pipe bytes);
  alt2 SM store; default IWT load 16-bit immediate into r[N] (instr.cpp:408-424).

### 2.7 GETC / RAMB / ROMB ($DF) and GETB ($EF)

- GETC (alt0): `colr = color(readROMBuffer())` (instr.cpp:372).
- RAMB (alt2,!alt1): syncRAMBuffer; `rambr = sr & 1` (instr.cpp:374-375).
- ROMB (alt3): syncROMBuffer; `rombr = sr & 0x7f` (instr.cpp:377-378).
- GETB family — see §2.4.

---

## 3. Memory map & bus

### 3.1 GSU-side view (`SuperFX::read/write`, ares memory.cpp:1-41)

The GSU's data bus decodes the 24-bit address:

| GSU address | Target | Gating | Cite |
|---|---|---|---|
| `$00–$3F:$0000–$FFFF` | ROM, mapped `((addr&0x3f0000)>>1)|(addr&0x7fff)` then `&romMask` | stalls while `!scmr.ron` | memory.cpp:2-9 |
| `$40–$5F:$0000–$FFFF` | ROM, linear `addr & romMask` | stalls while `!scmr.ron` | memory.cpp:11-18 |
| `$70–$71:$0000–$FFFF` | Game Pak RAM, `addr & ramMask` | stalls while `!scmr.ran` (read & write) | memory.cpp:20-27, 32-41 |
| (other) | open bus → returns passed `data` | — | memory.cpp:29 |

ROM/RAM-access stall: while the relevant access bit (ron/ran) is *not* granted to the
GSU, the read/write loops `step(6); synchronize(cpu); break if scheduler.synchronizing()`
(memory.cpp:3-6,12-15,21-24,34-37) — i.e. the GSU burns cycles waiting for the SNES to
grant access via SCMR. luna can model this as: if access not granted, stall the GSU
(yield to SNES) until SCMR flips the bit.

The first-bank ROM remap `((addr&0x3f0000)>>1)|(addr&0x7fff)` packs the upper-half
$8000–$FFFF of each LoROM bank into a contiguous ROM image; the `$40–$5F` window is the
flat/linear view of the same ROM. `romMask = romSizeRound(rom.size())-1` (rounded up to
a power of two to fix the non-power-of-two SuperFX voxel demo) (superfx.cpp:48-56,67).
`ramMask = ram.size()-1`, `bramMask = bram.size()-1` (superfx.cpp:68-69).

Mesen GSU-side mappings (mesen Gsu.cpp:67-72):
`$00–$3F:$8000–$FFFF` ROM, `$00–$3F:$0000–$7FFF` ROM **mirror**, `$40–$5F:$0000–$FFFF`
ROM, `$70–$71:$0000–$FFFF` GSU RAM. **Agrees with ares** (the $0000–$7FFF mirror is the
LoROM low-half mirror that ares folds into its remap formula).

### 3.2 SNES-CPU-side view (the SuperFX mapper)

ares wires three AbstractMemory shims (bus.cpp) into the SNES bus; the SNES board glue
(not in these files) places them. From Mesen's explicit CPU mappings (mesen Gsu.cpp:50-65),
authoritative for the address ranges:

| SNES address | Target | Cite |
|---|---|---|
| `$00–$3F:$3000–$3FFF` and `$80–$BF:$3000–$3FFF` | GSU MMIO (this = Gsu handler) | mesen Gsu.cpp:51-52 |
| `$00–$3F:$8000–$FFFF`, `$80–$BF:$8000–$FFFF` | ROM (via GsuRomHandler) | mesen Gsu.cpp:61-62 |
| `$40–$5F:$0000–$FFFF`, `$C0–$DF:$0000–$FFFF` | ROM (GsuRomHandler) | mesen Gsu.cpp:64-65 |
| `$00–$3E:$6000–$7FFF` (+$80 mirror) | Game Pak RAM (GsuRamHandler) | mesen Gsu.cpp:54-57 |
| `$70–$71:$0000–$FFFF`, `$F0–$F1:$0000–$FFFF` | Game Pak RAM (GsuRamHandler) | mesen Gsu.cpp:58-59 |

ares internal MMIO mirror is `$3000 | addr.bit(0,9)` → the live window is `$3000–$33FF`
mirrored up to `$34FF`; Mesen masks `addr & 0x33FF` (mesen Gsu.cpp:465,508). luna should
treat $3000–$34FF in $00–$3F:/$80–$BF as the MMIO region.

### 3.3 Bus-conflict behaviour while the GSU runs

When the GSU is running (`sfr.g`) **and** holds ROM access (`scmr.ron`), SNES reads of
the ROM return a **fixed vector**, not ROM data (ares bus.cpp:11-20):
```
vector[16] = {00,01,00,01,04,01,00,01,00,01,08,01,00,01,0c,01}; return vector[addr&15];
```
This is the canonical "GSU busy" reset/IRQ vector pattern the SNES sees so it doesn't
execute garbage. Mesen reproduces it in GsuRomHandler::Read (mesen GsuRomHandler.h:20-38):
odd addresses → 0x01, even addresses → {0,0,0x04,0,0,0,0x08,0,0x0C} per `addr&0x0E`,
gated on `SFR.Running && GsuRomAccess`. **Agrees.**

When the GSU runs **and** holds RAM access (`scmr.ran`), SNES reads of Game Pak RAM
return **open bus** (the passed `data`) (ares bus.cpp:36-38; mesen GsuRamHandler.h:20-27
returns 0 / "TODO open bus"). SNES writes still go through in ares (bus.cpp:41-43) but
Mesen drops SNES RAM writes while the GSU owns RAM (GsuRamHandler.h:42-47). Minor
divergence; ares lets the write land — **follow ares**.

BRAM (backup RAM) writes from the SNES are gated by `bramr` (ares bus.cpp:60-62).

---

## 4. Pixel-plot pipeline (ares core.cpp)

### 4.1 PixelCache structure

Two caches (`pixelcache[2]`, ares registers.hpp:163-167):
```
struct PixelCache { n16 offset; n8 bitpend; n8 data[8]; }
```
`offset` = tile-row position `(y<<5)+(x>>3)` (8-pixel run), `bitpend` = per-x written-bit
mask (8 bits), `data[8]` = color index per x in the run. pixelcache[0] = primary (current
8-pixel run), pixelcache[1] = secondary (one-deep writeback queue). Mesen mirrors with
`GsuPixelCache{ X; Y; Pixels[8]; ValidBits; }` (mesen GsuTypes.h:45-51) — note Mesen stores
X(&0xF8)/Y instead of a packed offset, but the geometry is identical.

### 4.2 color() — COLR/GETC color resolution (core.cpp:5-9)

```
if(por.highnibble)  return (colr & 0xf0) | (source >> 4);
if(por.freezehigh)  return (colr & 0xf0) | (source & 0x0f);
return source;
```
Used by COLOR ($4E alt0) and GETC ($DF alt0). Mesen GetColor identical
(mesen Gsu.Instructions.cpp:725-735).

### 4.3 plot() (core.cpp:11-46)

1. **Transparency test** (skip plot if transparent & `!por.transparent`):
   - md==3 (8bpp): if `por.freezehigh` test `(colr&0x0f)==0`, else test `colr==0`.
   - else: test `(colr&0x0f)==0`.
   (core.cpp:12-22) Mesen IsTransparentPixel uses bpp: 2→`(c&3)==0`, 4→`(c&0xf)==0`,
   8→`c==0`, with `c = freezehigh ? colr&0x0f : colr` (mesen Gsu.Instructions.cpp:646-656).
   **Divergence:** ares only special-cases md==3, testing low *nibble* in all non-8bpp
   modes (so 2bpp tests bits 3:0, not 1:0). Mesen tests exactly `bpp` low bits. This
   differs for 2bpp (md==0). **Follow ares** (core.cpp is the verified reference) — but
   flag as a candidate to revisit if a 2bpp-plot test ROM disagrees.
2. **Dither** (core.cpp:24-28): if `por.dither && md!=3`: if `(x^y)&1` use high nibble
   (`color>>=4`), then `color &= 0x0f`.
3. **Cache slot** (core.cpp:30-36): `offset=(y<<5)+(x>>3)`; if it differs from
   pixelcache[0].offset → flush pixelcache[1] to RAM, move [0]→[1], clear [0]
   (bitpend=0, offset=new).
4. **Write pixel** (core.cpp:38-44): `x=(x&7)^7` (bit-reverse within the run);
   `data[x]=color; bitpend |= 1<<x`; if bitpend==0xff (run full) flush [1], move [0]→[1],
   clear [0].

Mesen DrawPixel (Gsu.Instructions.cpp:658-682): same steps; uses `PrimaryCache.X!=(x&0xF8)
|| PrimaryCache.Y!=y` as the "new run" test (equivalent to offset compare). **Agrees.**

### 4.4 flushPixelCache() — writeback to Game Pak RAM (core.cpp:73-103)

Converts the run's `data[8]` planar color indices into bitplane bytes and writes them to
the tile at `0x700000 + cn*(bpp<<3) + (scbr<<10) + (y&7)*2`. Character number `cn` from the
tile-index formula (§4.6). For each of `bpp` planes `n`:
- `byte = ((n>>1)<<4) + (n&1)` → byte offsets {0,1,16,17,32,33,48,49} (core.cpp:90).
- Build `data` by gathering bit n of each `cache.data[x]` into bit x (core.cpp:92).
- If `bitpend != 0xff` (partial run): read-modify-write — `step(clsr?5:6)`,
  `data &= bitpend`, `data |= read(addr+byte) & ~bitpend` (core.cpp:93-97).
- `step(clsr?5:6); write(addr+byte, data)` (core.cpp:98-99).
- Clear bitpend (core.cpp:102).

Mesen WritePixelCache identical (Gsu.Instructions.cpp:693-723); Mesen adds a
`WaitForRamAccess()` before each plane write (line 718) which ares folds into the RAM
stall in write()/memory.cpp.

### 4.5 rpix() — pixel readback (core.cpp:48-71)

Flush both caches first (secondary then primary — core.cpp:49-50), then read the `bpp`
bitplane bytes at the same tile address and reassemble the color index of pixel (x,y):
- `x=(x&7)^7`; for each plane n: `byte=((n>>1)<<4)+(n&1)`; `step(clsr?5:6)`;
  `data |= ((read(addr+byte)>>x)&1)<<n` (core.cpp:62-68).
RPIX sets s,z from the result and writes dr (ares instr.cpp:132-134).
Mesen ReadPixel identical but **steps after the read** rather than before
(Gsu.Instructions.cpp:636-641) — cosmetic ordering, same cycle total.

### 4.6 Tile-index (character number) formula (core.cpp:53-58 / 80-84; mesen GetTileIndex 609-617)

Selector: `por.obj ? 3 : scmr.ht`:
```
ht 0: cn = ((x&0xf8)<<1) + ((y&0xf8)>>3)
ht 1: cn = ((x&0xf8)<<1) + ((x&0xf8)>>1) + ((y&0xf8)>>3)
ht 2: cn = ((x&0xf8)<<1) + ((x&0xf8)<<0) + ((y&0xf8)>>3)
ht 3: cn = ((y&0x80)<<2) + ((x&0x80)<<1) + ((y&0x78)<<1) + ((x&0x78)>>3)   // OBJ/160-px
```
Tile byte address: `0x700000 + cn*(bpp<<3) + (scbr<<10) + (y&7)*2` (core.cpp:60,87).
Mesen GetTileAddress: `(0x700000 | (ScreenBase<<10)) + tileIndex*(PlotBpp<<3) + (y&7)*2`
(Gsu.Instructions.cpp:620-624). **Agrees** (`ScreenBase`==scbr).

`bpp = 2 << (md - (md>>1))` → md{0,1,2,3}→bpp{2,4,4,8} (core.cpp:59); `(bpp<<3)` = bytes
per tile (16/32/32/64). Bitplane interleave {0,1,16,17,32,33,48,49} is standard SNES
planar layout (planes 0/1 adjacent, planes 2/3 at +16, etc.).

---

## 5. Cache RAM (512-byte instruction cache)

Structure: `cache.buffer[512]`, `cache.valid[32]` (ares registers.hpp:158-161). 32 lines ×
16 bytes. `cbr` is the cache base PC (bits 15:4). Mesen: `_cache[512]`, `_cacheValid[32]`
(mesen Gsu.h:28-29).

**Fetch path** `readOpcode(address)` (ares memory.cpp:43-71):
1. `offset = address - cbr` (memory.cpp:44).
2. If `offset < 512` (in-cache window):
   - line = `offset>>4`. If `!valid[line]`: fill the 16-byte line — `dp = offset & 0xfff0`,
     `sp = (pbr<<16) + ((cbr+dp)&0xfff0)`; loop 16: `step(clsr?5:6); buffer[dp++]=read(sp++)`;
     then `valid[line]=true` (memory.cpp:46-53).
   - else (hit): `step(clsr?1:2)` (memory.cpp:55).
   - return `buffer[offset]` (memory.cpp:57).
3. Else (outside cache window): if `pbr<=0x5f` syncROMBuffer else syncRAMBuffer; then
   `step(clsr?5:6); return read((pbr<<16)|address)` (memory.cpp:60-70).

Mesen equivalent: ReadProgramByte (Gsu.cpp:307-330) + InitProgramCache (Gsu.cpp:271-290).
Mesen fills the line then `Step(clsr?5*16:6*16)` (one bulk step of 16× the per-byte cost,
Gsu.cpp:287) — same total. Mesen hit cost `Step(clsr?1:2)` (Gsu.cpp:316). **Agrees.**

**Cache addressing for MMIO ($3100–$32FF):** `(address + cbr) & 511` (ares memory.cpp:92,97).
Writing the 16th byte of a line (`(address&15)==15`) marks that line valid (memory.cpp:99) —
this is how the SNES can preload the cache. Mesen identical (Gsu.cpp:496-498, 584-590).

**Invalidation / flush** `flushCache()` = set all 32 valid flags false (ares memory.cpp:87-89).
Triggered by:
- CACHE op when cbr changes (instr.cpp:19-22).
- LJMP (instr.cpp:280-281).
- MMIO write to PBR $3034 (io.cpp:94).
- g 1→0 transition on $3030 write (also zeroes cbr) (io.cpp:78-81).
Mesen InvalidateCache mirrors all four triggers (Gsu.cpp:364-367, 552, 543-546; CACHE op
Gsu.Instructions.cpp:26-29; LJMP 103-104).

---

## 6. Timing model

### 6.1 Clock select & step granularity

`clsr` selects fast (21.4 MHz, clsr=1) vs slow (10.7 MHz, clsr=0). Throughout, the per-event
cost is `clsr ? fast : slow`. The two base values are **F=5 / S=6** for memory cycles and
**F=1 / S=2** for cache-hit fetch cycles. `step(clocks)` (ares timing.cpp:1-19):
1. Service pending ROM buffer: `romcl -= min(clocks,romcl)`; when it hits 0, `sfr.r=0`,
   `romdr = read((rombr<<16)+r14)` (timing.cpp:2-8).
2. Service pending RAM write: `ramcl -= min(clocks,ramcl)`; when 0, `write(0x700000 +
   (rambr<<16) + ramar, ramdr)` (timing.cpp:10-15).
3. `Thread::step(clocks); Thread::synchronize(cpu)` — advance the coprocessor clock and
   re-sync to the SNES (timing.cpp:17-18).

Mesen Step (Gsu.cpp:428-448) is the same: accumulate CycleCount, decrement RomDelay/RamDelay,
fire the buffered ROM read / RAM write on reaching 0.

### 6.2 ROM buffer (R14-triggered)

- `updateROMBuffer()`: `sfr.r=1; romcl = clsr?5:6` (ares timing.cpp:30-33). Armed when R14 is
  written (superfx.cpp:35-38, io.cpp:68).
- `syncROMBuffer()`: if `romcl` pending, `step(romcl)` to force completion (timing.cpp:21-23).
- `readROMBuffer()`: syncROMBuffer then return `romdr` (timing.cpp:25-28).
Used by GETB/GETC/ROMB.

### 6.3 RAM buffer (delayed write)

- `writeRAMBuffer(addr,data)`: syncRAMBuffer first (drain prior pending write), then
  `ramcl = clsr?5:6; ramar=addr; ramdr=data` (ares timing.cpp:44-48). The actual store lands
  `ramcl` cycles later in step() (timing.cpp:11-15).
- `readRAMBuffer(addr)`: syncRAMBuffer then `read(0x700000 + (rambr<<16) + addr)`
  (timing.cpp:39-42). **Reads are not buffered** — only writes are delayed.
- `syncRAMBuffer()`: `if(ramcl) step(ramcl)` (timing.cpp:35-37).

Mesen: WriteRam arms RamDelay/RamWriteAddress/RamWriteValue (Gsu.cpp:419-426);
ReadRamBuffer drains then reads directly (Gsu.cpp:412-417). **Agrees.**

### 6.4 Per-instruction explicit cycle costs (step() calls inside ops)

Most ops cost only their fetch (cache hit F1/S2, or fill/non-cache F5/S6). Ops that add
explicit `step()`:

| Op | extra step | Cite |
|---|---|---|
| MULT/UMULT | if `!ms0`: `clsr?1:2` | instr.cpp:228 |
| FMULT/LMULT | `(ms0?3:7) * (clsr?1:2)` | instr.cpp:304 |
| RPIX | per plane (bpp): `clsr?5:6` | core.cpp:66 |
| PLOT cache flush | per plane, +RMW when partial: each `clsr?5:6` | core.cpp:94,98 |
| cache line fill | 16 × `clsr?5:6` | memory.cpp:50 |
| non-cache fetch | `clsr?5:6` | memory.cpp:63,68 |
| cache hit fetch | `clsr?1:2` | memory.cpp:55 |
| ROM/RAM access stall | `6` per spin while access denied | memory.cpp:4,13,22,35 |

### 6.5 Lockstep with the SNES CPU

The SuperFX is a `Thread` (ares superfx.hpp:1); every `step()` ends with
`Thread::synchronize(cpu)` (timing.cpp:18), and the GSU's `main()` loop runs one instruction
per dispatch, idling `step(6)` per call when `sfr.g==0` (superfx.cpp:28-29). Any SNES MMIO
access calls `cpu.synchronize(*this)` first (io.cpp:2,54) to align clocks before touching
state. Mesen drives it from `Run()` (mesen Gsu.cpp:89-100): execute until
`CycleCount >= masterClock*clockMultiplier`, then a final Step to align; this is the
catch-up model rather than ares' interleaved-thread model. luna's existing coproc scheduler
should drive the GSU like SA-1 — run N GSU cycles per SNES master-clock budget, with a
synchronize on every MMIO touch.

`Frequency` (ares superfx.hpp:90) is the GSU thread base clock; the clsr fast/slow ratio is
expressed through the per-event step counts, not by changing Frequency. Mesen multiplies the
master clock by `_clockMultiplier = GsuClockSpeed/100` (Gsu.cpp:26, 91) — an overclock knob,
default 1; **not hardware**, ignore for accuracy.

---

## 7. Control flow / handshake

### 7.1 Start (GO)

The SNES launches the GSU by writing R15 (the PC) via MMIO, **high byte last**: writing
$301F (R15 high byte) sets `sfr.g=1` as a side effect (ares io.cpp:70). So the canonical
launch is: set up registers, write R15 low ($301E) then R15 high ($301F) → GO. Alternatively
the SNES can set sfr.g via the $3030 SFR write, but the documented trigger is the R15-high
write. Mesen: writing $301F sets `SFR.Running=true; UpdateRunningState()` (Gsu.cpp:528-531);
also reg==14 write arms the ROM buffer (Gsu.cpp:525-527).

On launch the GSU begins fetching from `(pbr<<16)|r15` through the cache (memory.cpp:43-71).
`pipeline` starts as 0x01 (NOP) so the first executed instruction is a NOP (gsu.cpp:34) —
gives the pipeline one slot to prime.

### 7.2 Stop / HALT

- STOP op ($00): `if(!cfgr.irq){ sfr.irq=1; stop(); }` then `sfr.g=0; pipeline=NOP; reset()`
  (ares instr.cpp:2-10). `stop()` → `cpu.irq(1)` raises the SNES IRQ (core.cpp:1-3).
- The SNES can also clear g by writing $3030 with bit5=0; the g 1→0 transition resets
  `cbr=0` and flushes the cache (io.cpp:76-82).

While `sfr.g==0`, `main()` just idles `step(6)` and does not fetch/execute (superfx.cpp:28-29).

### 7.3 IRQ generation & acknowledge

- STOP raises IRQ unless masked by `cfgr.irq` (the "IRQ disable" bit): only when
  `cfgr.irq==0` does STOP set `sfr.irq=1` and call `stop()`→`cpu.irq(1)` (instr.cpp:3-6,
  core.cpp:1-3). (Note the inverted sense: `cfgr.irq==1` *masks* the interrupt.)
- The SNES acknowledges by **reading SFR high byte $3031**, which clears `sfr.irq` and
  deasserts the line: `regs.sfr.irq=0; cpu.irq(0)` (io.cpp:18-22).
Mesen: STOP sets `SFR.Irq` + `SetIrqSource(Coprocessor)` when `!IrqDisabled`
(Gsu.Instructions.cpp:6-17); $3031 read clears Irq + `ClearIrqSource` (Gsu.cpp:481-486).
**Agrees.**

### 7.4 Reset state

See §1.8. Key: vcr=0x04, pipeline=0x01 (NOP), sfr.g=0 (halted), all banks 0, cache invalid,
pixelcache offsets ~0.

---

## 8. ares ↔ Mesen2 divergences (summary)

| # | Topic | ares | Mesen2 | Follow |
|---|---|---|---|---|
| 1 | $06/$07 branch mnemonic | $06=blt label, $07=bge label (cond by `(s^ov)`) | $06=BGE, $07=BLT | **Conditions identical** — implement by condition per byte; mnemonic is cosmetic |
| 2 | plot() transparency test for non-8bpp | tests low **nibble** `(colr&0x0f)==0` for all md≠3 | tests exactly `bpp` low bits (2bpp→`&3`) | **ares** (verified core.cpp); revisit if a 2bpp test ROM fails |
| 3 | MMIO lockout while running | none — all regs accessible | blocks all but $3030/$3031/$303B read, $3030/$303A write | **ares** (no lockout); Mesen behavior is a documented nicety |
| 4 | SNES RAM write while GSU owns RAM | write lands (bus.cpp:41-43) | dropped (GsuRamHandler.h:42-47) | **ares** (let write land) |
| 5 | RPIX / cache-flush step ordering | step before read | step after read | Cosmetic; same total cycles — either ok, prefer ares ordering |
| 6 | MERGE flag assignment order | ov,s,cy,z | cy,ov,s,z | Same masks/results; no behavioral difference |
| 7 | Clock multiplier | none (hardware) | `GsuClockSpeed/100` overclock knob | Ignore (Mesen UI feature) |
| 8 | Cache fill stepping | 16 separate `step(F5/S6)` | one `Step(16×)` bulk | Same total; ares interleaves sync per byte |

No *semantic* opcode-level disagreements were found: every ALU result, flag rule, prefix
rule, plot geometry, and timing constant matches between the two references. The divergences
above are all either cosmetic (naming/ordering) or peripheral-policy (access gating), with
the one genuine edge case being #2 (2bpp transparency test), where ares is the chosen
authority.
