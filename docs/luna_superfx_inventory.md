# Luna SuperFX (GSU) Implementation Inventory

## Summary

This document maps luna's coprocessor and mapper architecture, using SA-1 as the reference pattern for SuperFX integration. The document identifies all seams where a new coprocessor plugs in: mapper enum, bus shim, coproc state struct, scheduler step hook, MMIO dispatch, cartridge detection, and test harness.

---

## 1. MapperKind Enum Definition

**File:** `/home/kobenairb/workspace/luna/crates/luna-bus/src/mapper.rs:10-27`

```rust
pub enum MapperKind {
    LoRom,
    HiRom,
    ExHiRom,
    Sa1,
    SuperFx,        // ← Already defined as a variant (line 22)
    Sdd1,
    Spc7110,
}
```

**Status:** ✅ SuperFX variant **already exists** in the enum at line 22. No definition required.

**All MapperKind usages** (matched/handled):
- **crates/luna-bus/src/mapper.rs:78** — `sa1_snapshot()` method (trait, SA-1 only, returns None for others)
- **crates/luna-bus/src/mapper.rs:150** — `from_bytes_forced` cartridge parsing routes to LoROM header offset for SuperFX

---

## 2. Cartridge Detection Logic

**File:** `/home/kobenairb/workspace/luna/crates/luna-cartridge/src/lib.rs:232-239`

**Current mapper detection code:**
```rust
const fn mapper_from_byte(byte: u8) -> Option<MapperKind> {
    match byte & 0x0F {
        0 => Some(MapperKind::LoRom),
        1 => Some(MapperKind::HiRom),
        3 => Some(MapperKind::Sa1),
        5 => Some(MapperKind::ExHiRom),
        _ => None,
    }
}
```

**Status:** ⚠️ **SuperFX is NOT detected from ROM headers yet.** The function only recognizes mapper nibbles `0, 1, 3, 5`. 

Per the SNES hardware spec, SuperFX ROMs use chipset byte `$00:FFD6 = 0xDD` (the enhancement ID). Luna currently does **not** parse or check that byte. The header parsing at `parse_at` (line 196) reads the mapper byte but ignores the enhancement/chipset byte entirely.

**What needs to happen:**
- Add parser logic to read `$00:FFD6` (enhancement/chipset byte)
- Map chipset byte `0xDD` → `MapperKind::SuperFx`
- Alternatively, infer from title patterns (Star Fox, Yoshi's Island) for robustness

**Known SuperFX games** (chipset byte `0xDD`):
- Star Fox (SMRPG equivalent: 32 MB ROM, fast ROM)
- Yoshi's Island (48+ MB ROM)
- Doom (48 MB ROM)
- Winter Gold
- Super Mario World 2: Yoshi's Island (Japan variant)

**Reference:** `crates/luna-cartridge/src/lib.rs:196-230` reads the internal header at the inferred offset (`HEADER_OFFSET_LOROM` = `0x7FC0` for LoROM-region layouts, which includes SuperFX).

---

## 3. LoROM Mapper Shim (Template for SuperFX Shim)

**File:** `/home/kobenairb/workspace/luna/crates/luna-bus/src/lorom.rs:37-72`

**LoROM address-mapping math** (the simplest template):

```rust
/// LoROM: 32 KB ROM pages at $8000-$FFFF, mirrored across banks.
fn rom_offset(&self, bank: u8, offset: u16) -> Option<usize> {
    if offset < 0x8000 {
        return None;  // Only ROM at $8000-$FFFF
    }
    let normalized_bank = bank & 0x7F;  // Mirror $80-$FF → $00-$7F
    let rom_offset = (usize::from(normalized_bank) * 0x8000) 
                     + (usize::from(offset) - 0x8000);
    if rom_offset < self.rom.len() { Some(rom_offset) } else { None }
}

/// SRAM at $70-$7D / $F0-$FD, offsets $0000-$7FFF.
fn sram_offset(&self, bank: u8, offset: u16) -> Option<usize> {
    if self.sram.is_empty() { return None; }
    let is_sram_bank = matches!(bank, 0x70..=0x7D | 0xF0..=0xFD);
    if !is_sram_bank || offset >= 0x8000 { return None; }
    let normalized_bank = (bank & 0x7F) - 0x70;
    let sram_offset = usize::from(normalized_bank) * 0x8000 + usize::from(offset);
    Some(sram_offset % self.sram.len())
}
```

**Mapper trait it implements:**
- `/home/kobenairb/workspace/luna/crates/luna-bus/src/mapper.rs:34-104`
- Trait methods: `kind()`, `read()`, `write()`, `rom_size()`, `sram_size()`
- Optional methods: `step_coproc()` (default no-op), `coproc_main_irq_pending()` (default false), `sa1_snapshot()` (default None)

**The Mapper trait read/write signatures:**
```rust
pub trait Mapper {
    fn read(&mut self, addr: Addr24) -> Option<u8>;      // ROM/SRAM read
    fn write(&mut self, addr: Addr24, value: u8) -> bool;  // returns true if accepted
    fn step_coproc(&mut self, main_mclk: u32) {}          // advance coprocessor
    fn coproc_main_irq_pending(&self) -> bool { false }   // IRQ line level
    fn sa1_snapshot(&self) -> Option<Sa1Snapshot> { None }
}
```

---

## 4. SA-1 Coprocessor Wiring (Pattern to Mirror)

### 4.1 Chip-side State Struct

**File:** `/home/kobenairb/workspace/luna/crates/luna-core/src/coproc/sa1.rs:30-56`

```rust
pub struct Sa1Chip {
    /// Shared memory state (ROM, I-RAM, BW-RAM, MMIO registers).
    inner: Sa1Mapper,
    /// The SA-1's own 65C816 instance.
    pub cpu: Cpu,
    /// false while held in reset (CCNT.7 = 1). Default false; main CPU releases it.
    pub running: bool,
    /// Sub-master-clock budget: SA-1 runs at ~6 mclk per instruction.
    deficit: u32,
    /// Optional SA-1-side execution log (MMIO accesses with SA-1 PC).
    sa1_side_log: Option<Vec<Sa1SideEvent>>,
    /// Optional full SA-1 instruction trace (pre-opcode register snapshots).
    sa1_trace: Option<(Vec<Sa1TraceEvent>, usize)>,
}

const MCLK_PER_SA1_INSN: u32 = 6;  // Line 58
```

**Construction:** `Sa1Chip::new(Sa1Mapper)` at line 61-71.

**Reset vector loading:**
Lines 76-83 show how the SA-1 CPU loads its PC from the MMIO CRV register (`$2203/$2204`) when released from reset:
```rust
fn load_reset_vector(&mut self) {
    let lo = self.inner.read(make_addr(0x00, 0x2203)).unwrap_or(0);
    let hi = self.inner.read(make_addr(0x00, 0x2204)).unwrap_or(0);
    self.cpu.pc = u16::from(lo) | (u16::from(hi) << 8);
    self.cpu.pb = 0;
    self.cpu.stopped = false;
    self.cpu.waiting = false;
}
```

**Mapper trait delegation:** Lines 86-138 — Sa1Chip wraps Sa1Mapper and delegates `Mapper` methods.

### 4.2 Bus-side Mapper Shim

**File:** `/home/kobenairb/workspace/luna/crates/luna-bus/src/sa1.rs:1-100` (partial read)

The SA-1 bus shim (`Sa1Mapper`) is ~600 lines. It owns:
- ROM, BW-RAM (0..256 KB), I-RAM (2 KB), MMIO register file (512 bytes)
- Four super-bank selectors (`CXB`, `DXB`, `EXB`, `FXB` at `$2220-$2223`)
- BW-RAM window select (`BMAPS` at `$2224`)
- Hardware multiplier/divider state
- IRQ message latches and timer

The `Sa1Mapper` implements `Mapper` with SA-1-specific address translation that interprets the super-bank registers to select which 1 MB window of ROM is visible in each 256 KB CPU quarter.

### 4.3 Coproc Module Exposure

**File:** `/home/kobenairb/workspace/luna/crates/luna-core/src/coproc/mod.rs:1-14`

```rust
pub mod sa1;
pub use sa1::Sa1Chip;
```

**Status:** SA-1 is the only coprocessor currently implemented and re-exported. SuperFX would add:
```rust
pub mod superfx;  // To be created
pub use superfx::SuperFxChip;  // To be created
```

### 4.4 Scheduler Step Hook

**File:** `/home/kobenairb/workspace/luna/crates/luna-core/src/snes.rs:550-682` (step method)

**Key relevant lines:**
- Line 635: `cpu.step(&mut bus)` — main CPU executes one instruction via the bus
- **Line 663-665:** Coprocessor IRQ line check after instruction:
  ```rust
  if self.mapper.coproc_main_irq_pending() {
      self.cpu.trigger_irq();
  }
  ```

**The actual per-access coprocessor advance happens inside `Bus::io_cycle`:**

**File:** `/home/kobenairb/workspace/luna/crates/luna-core/src/snes.rs:1304-1364`

Inside `Bus::io_cycle` (called by every CPU memory access):
```rust
fn io_cycle(&mut self, mcycles: MCycles) {
    self.advance_time(mcycles, true);  // advance_coproc = true
}

fn advance_time(&mut self, mcycles: MCycles, advance_coproc: bool) {
    // ...
    if advance_coproc {
        self.mapper.step_coproc(step as u32);  // Line 1353
    }
    // ...
}
```

**So the coprocessor scheduler hook is:**
- **Primary:** `Mapper::step_coproc(main_mclk: u32)` called mid-instruction during every CPU memory access
- **Secondary:** `Mapper::coproc_main_irq_pending()` polled after each instruction to latch IRQ lines

For SA-1: `step_coproc` advances the SA-1 CPU at ~6 mclk per instruction (line 139-185 of sa1.rs).

### 4.5 SA-1-side Bus

**File:** `/home/kobenairb/workspace/luna/crates/luna-core/src/coproc/sa1.rs:225-326`

The `Sa1Bus` struct wraps the SA-1Mapper and provides a custom `Bus` implementation for the SA-1 CPU:

```rust
struct Sa1Bus<'a> {
    mapper: &'a mut Sa1Mapper,
    log: Option<&'a mut Vec<Sa1SideEvent>>,
    sa1_pc: u32,
}

impl Bus for Sa1Bus<'_> {
    fn read(&mut self, addr: Addr24) -> u8 { ... }
    fn write(&mut self, addr: Addr24, value: u8) { ... }
    fn nmi_pending(&self) -> bool { self.mapper.sa1_nmi_line() }
    fn irq_pending(&self) -> bool { self.mapper.sa1_irq_line() }
}
```

The `Sa1Bus::read/write` dispatch through `mapper.read_from_sa1()` / `mapper.write_from_sa1()` to route the SA-1's I-RAM mirror (`$0000-$07FF` → `$3000-$37FF`) correctly.

### 4.6 MMIO Dispatch Path (CPU-side → SA-1)

**File:** `/home/kobenairb/workspace/luna/crates/luna-core/src/snes.rs:1495-1507`

When the main CPU reads/writes to an address that the mapper claims (via `mapper.read(addr)` returning `Some(value)`), the bus logs it if SA-1 tracing is enabled:

```rust
if let Some(v) = self.mapper.read(addr) {
    if let Some(reg) = Self::sa1_reg(addr) {  // $2200-$23FF check
        if let Some(log) = self.sa1_log.as_mut() {
            log.push(Sa1LogEvent {
                mclk_total: *self.mclk_total,
                pc_full: self.cpu_pc_full,
                kind: MailboxEventKind::Read,
                reg,
                value: v,
            });
        }
    }
    return v;
}
```

**The dispatcher check** (`sa1_reg`) is at lines 1513-1525:
```rust
const fn sa1_reg(addr: Addr24) -> Option<u16> {
    let bank = (addr >> 16) as u8;
    let off = addr as u16;
    let bank_ok = bank <= 0x3F || (bank >= 0x80 && bank <= 0xBF);
    if bank_ok && off >= 0x2200 && off <= 0x23FF {
        Some(off)
    } else {
        None
    }
}
```

So **any read/write to `$00-$3F/$80-$BF:$2200-$23FF` that the mapper accepts gets logged as an SA-1 access**.

---

## 5. Top-level Snes Struct Mapper Field

**File:** `/home/kobenairb/workspace/luna/crates/luna-core/src/snes.rs:25-127`

```rust
pub struct Snes {
    pub cpu: Cpu,
    pub ppu: Ppu,
    pub dma: Dma,
    pub cpu_regs: CpuRegs,
    pub wram: Box<[u8; 0x20000]>,
    pub mapper: Box<dyn Mapper + Send>,  // Line 38: dynamic dispatch
    pub fast_rom: bool,
    pub nmi_pending: bool,
    pub irq_pending: bool,
    pub total_mclk: MCycles,
    pub apu_real: Apu,
    // ... APU, scheduler state, logging fields
}
```

**No separate SA-1 or SuperFX field exists.** Both are wrapped *inside* the `mapper` (as concrete `Sa1Chip` or future `SuperFxChip`), which erases the concrete type via the `Mapper` trait object.

### 5.1 Mapper Construction in `from_cartridge`

**File:** `/home/kobenairb/workspace/luna/crates/luna-core/src/snes.rs:311-330`

```rust
pub fn from_cartridge(cart: Cartridge) -> Self {
    let sram_bytes = (cart.header.sram_size_kb as usize) * 1024;
    let region = cart.header.region;
    let mapper: Box<dyn Mapper + Send> = match cart.header.mapper_kind {
        MapperKind::LoRom => Box::new(LoRomMapper::new(cart.rom, sram_bytes)),
        kind @ (MapperKind::HiRom | MapperKind::ExHiRom) => {
            Box::new(HiRomMapper::with_kind(kind, cart.rom, sram_bytes))
        }
        MapperKind::Sa1 => Box::new(Sa1Chip::new(Sa1Mapper::new(cart.rom, sram_bytes))),
        other => {
            panic!(
                "Cartridge requires coprocessor support not yet implemented: {other:?}. \
                 Super FX / S-DD1 / SPC7110 will land in their own dedicated phases."
            );
        }
    };
    // ... rest of Snes initialization
}
```

**For SuperFX, the match arm would be:**
```rust
MapperKind::SuperFx => Box::new(SuperFxChip::new(SuperFxMapper::new(cart.rom, sram_bytes))),
```

### 5.2 Scheduler Integration

The scheduler advances **all** coprocessors uniformly via the `Mapper` trait:

1. **Per-memory-access:** `Bus::io_cycle` → `advance_time` → `mapper.step_coproc(mcycles)` (line 1353)
2. **Per-instruction:** After `cpu.step()`, check `mapper.coproc_main_irq_pending()` to latch IRQ (line 663)

No per-coprocessor special case needed; the Mapper trait method is the hook.

---

## 6. Luna-API State Surface

**File:** `/home/kobenairb/workspace/luna/crates/luna-api/src/lib.rs:68-104`

```rust
pub struct EmulatorState {
    pub rom: Option<RomInfo>,
    pub cpu: CpuState,
    pub ppu: PpuState,
    pub cpu_regs: CpuRegsState,
    pub scheduler: SchedulerState,
    pub apu: ApuState,
    pub stats: Stats,
    /// SA-1 coprocessor CPU state, if the loaded cartridge hosts one.
    /// None for non-SA-1 carts.
    pub sa1: Option<Sa1State>,  // Lines 86-90
}

pub struct Sa1State {
    pub pc: u16,
    pub pb: u8,
    pub p: u8,
    pub running: bool,
}
```

**Status:** SA-1 is surfaced to the API via `mapper.sa1_snapshot()` method. The mapper trait has:

**File:** `/home/kobenairb/workspace/luna/crates/luna-bus/src/mapper.rs:74-80`

```rust
/// Snapshot the SA-1 coprocessor's CPU state, if this mapper hosts one.
/// Plain LoROM / HiROM / Super FX / DSP-N return None.
fn sa1_snapshot(&self) -> Option<Sa1Snapshot> {
    None
}
```

SuperFX (and other future coprocs) would **not** have equivalent snapshot methods yet. For SuperFX to be debuggable, you'd either:

1. Add a `superfx_snapshot()` method to the Mapper trait (API-breaking change) and a `superfx: Option<SuperFxState>` field to EmulatorState.
2. Or implement a generic `coproc_snapshot(kind: MapperKind)` that returns an enum over all coproc states.
3. Or leave SuperFX state inaccessible via the public API until a later phase.

**Current approach (SA-1 only):** Diagnostic access to SA-1 PC/running state without a general coproc interface.

---

## 7. Test Harness

**File:** `/home/kobenairb/workspace/luna/crates/luna-core/tests/snes_test_roms.rs:90-140`

The test harness:
1. Loads a ROM (forced to a specific mapper for headerless test ROMs)
2. Calls `Snes::from_cartridge()` to construct the machine
3. Calls `snes.reset()` to load the reset vector
4. Runs `snes.step()` in a loop until the framebuffer settles (identical for N=8 samples)
5. SHA-256 hashes the final framebuffer and compares against a golden baseline

**Relevant snippet:**
```rust
fn run_to_stable(rom: Vec<u8>, hold: u16) -> Vec<u8> {
    let mut cart = Cartridge::from_bytes_forced(rom, MapperKind::LoRom).expect("forced LoROM load");
    cart.header.region = luna_cartridge::Region::Pal;  // Force PAL for Peter Lemon suite
    let mut snes = Snes::from_cartridge(cart);
    snes.reset();
    // ... run loop ...
}
```

For SuperFX tests, you would:
1. Add test ROMs (Star Fox, Yoshi's Island) to the corpus at `../luna_tests/superfx/`
2. Add a test function that loads them and verifies the framebuffer hash matches a golden PNG
3. Or add them to the parametrized `snes_test_roms!` macro at the file's bottom

**Example golden-hash test framework:**
```rust
#[test]
fn star_fox_renders_correctly() {
    if let Some(corpus) = corpus_root() {
        let rom = std::fs::read(corpus.join("superfx/star_fox.sfc")).ok();
        if let Some(rom) = rom {
            let fb = run_to_stable(rom, 0);
            let hash = hex(&Sha256::digest(&fb));
            assert_eq!(hash, "deadbeef...");  // Golden hash
        }
    }
}
```

---

## 8. Existing SuperFX Bits (Partial)

### 8.1 Mapper enum variant (exists)
- `/home/kobenairb/workspace/luna/crates/luna-bus/src/mapper.rs:22` — SuperFx variant declared

### 8.2 Cartridge header offset routing (exists)
- `/home/kobenairb/workspace/luna/crates/luna-cartridge/src/lib.rs:150` — SuperFx routes to LoROM header offset (`0x7FC0`)
- This is correct (Star Fox etc. use LoROM-region layout despite the GSU add-on)

### 8.3 Panic on load (exists)
- `/home/kobenairb/workspace/luna/crates/luna-core/src/snes.rs:324-329` — Any coprocessor besides LoRom/HiRom/ExHiRom/SA-1 panics with a message mentioning SuperFX

---

## 9. Implementation Checklist for SuperFX

| Component | File | Status | Notes |
|-----------|------|--------|-------|
| **MapperKind enum** | luna-bus/mapper.rs | ✅ Done | Variant exists at line 22 |
| **ROM header detection** | luna-cartridge/lib.rs | ❌ TODO | Need to parse chipset byte `$00:FFD6 = 0xDD` |
| **Bus mapper shim** | luna-bus/superfx.rs | ❌ TODO | Create struct, implement `Mapper` trait (address translation for super-banks) |
| **Chip-side coprocessor** | luna-core/coproc/superfx.rs | ❌ TODO | Wraps SuperFxMapper + own 65C816, implements `Mapper` trait |
| **Module export** | luna-core/coproc/mod.rs | ❌ TODO | Add `pub mod superfx; pub use superfx::SuperFxChip;` |
| **Snes::from_cartridge** arm | luna-core/snes.rs | ❌ TODO | Add `MapperKind::SuperFx => Box::new(SuperFxChip::new(...))` |
| **Luna-API snapshot** | luna-api/lib.rs | ⚠️ Optional | Add `superfx: Option<SuperFxState>` or defer until diagnostics needed |
| **Test ROMs** | crates/luna-core/tests/snes_test_roms.rs | ⚠️ Optional | Add Star Fox / Yoshi's Island golden-hash tests (requires ROM corpus) |

---

## 10. SuperFX Hardware Overview (for reference)

The SuperFX (GSU — Graphics Support Unit) is a 10.74 MHz 16-bit custom CPU used in Star Fox, Yoshi's Island, and a handful of other games.

**Key differences from SA-1:**
- **Single coprocessor**: No cross-CPU handshake DMA like SA-1; instead, SuperFX fills framebuffer RAM directly
- **ROM banking**: Uses LoROM-style 32 KB pages + super-bank registers (similar to SA-1 but simpler)
- **Internal RAM**: 512 bytes (vs SA-1's 2 KB I-RAM)
- **No character-conversion DMA**: No CC1/CC2 (unlike SA-1)
- **GSU registers**: Control register at `$00-$3F:$3000-$32FF` (vs SA-1's `$2200-$23FF`)
- **IRQ mechanism**: Level-driven like SA-1; can IRQ main CPU when operations complete
- **Pipeline**: Parallel ROM/RAM fetching + execution (3-stage pipeline)

In luna, SuperFX would follow the exact SA-1 pattern:
1. `SuperFxMapper` (bus-side, owns ROM/SRAM/IRAM/MMIO, implements address translation)
2. `SuperFxChip` (coprocessor-side, wraps `SuperFxMapper` + owns own 65C816, implements the `Mapper` trait for delegation)
3. Both plugged into `Snes` via the `Mapper` trait object

---

## File Reference Summary

| File | Purpose | Seam for SuperFX |
|------|---------|------------------|
| luna-bus/mapper.rs | Mapper trait definition | ✅ SuperFx variant already in enum; may need `superfx_snapshot()` if API exposure needed |
| luna-bus/lorom.rs | LoROM shim template | Template for SuperFxMapper address math |
| luna-bus/sa1.rs | SA-1 bus shim | **Template pattern** for SuperFxMapper (super-banks, MMIO, ROM/RAM routing) |
| luna-cartridge/lib.rs | ROM detection | ❌ Must add chipset byte parsing for SuperFx detection |
| luna-core/snes.rs | Top-level machine | ❌ Must add `MapperKind::SuperFx` arm in `from_cartridge` match |
| luna-core/coproc/mod.rs | Coprocessor module | ❌ Must export SuperFxChip |
| luna-core/coproc/sa1.rs | SA-1 chip | **Template pattern** for SuperFxChip structure and scheduler hook |
| luna-api/lib.rs | Public API state | ⚠️ Consider `superfx: Option<SuperFxState>` for debugger parity with SA-1 |
| luna-core/tests/snes_test_roms.rs | Test harness | ⚠️ Future home for Star Fox / Yoshi's Island regression tests |

