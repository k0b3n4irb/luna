# Mesen2 SNES Event Viewer — Reference Spec (port target for luna)

Study of Mesen2 master (raw.githubusercontent.com / SourMesen/Mesen2). This
is the diff target for a faithful ("à l'identique") port. No luna code here.

Sources read (file:line cited inline):
- `Core/Debugger/BaseEventManager.h` / `.cpp` — base capture + overlay engine.
- `Core/SNES/Debugger/SnesEventManager.h` / `.cpp` — SNES specifics.
- `Core/Debugger/DebugTypes.h` — `DebugEventType`, `MemoryOperationInfo`.
- `UI/Config/Debugger/SnesEventViewerConfig.cs` — category default colors + toggles.
- `UI/Config/Debugger/EventViewerCategoryCfg.cs` — `{ Color, Visible=true }`.
- `UI/Debugger/Labels/DefaultLabelHelper.cs` — SNES register-name table.

NOTE on paths: includes resolve relative to `Core/`, so the base manager is
at `Core/Debugger/BaseEventManager.{h,cpp}` (NOT `Core/Shared/Debugger/`).

---

## A) Event capture model

### A.1 The `DebugEventInfo` struct (`BaseEventManager.h`)

```
struct DebugEventInfo {
    MemoryOperationInfo Operation;      // Address, Value, Type, MemType
    DebugEventType      Type;
    uint32_t            ProgramCounter;
    int16_t             Scanline;
    uint16_t            Cycle;          // = H-clock (0..1363)
    int16_t             BreakpointId = -1;
    int8_t              DmaChannel   = -1;
    DmaChannelConfig    DmaChannelInfo;
    uint32_t            Flags;          // EventFlags bitmask
    int32_t             RegisterId   = -1;
    MemoryOperationInfo TargetMemory;
    uint32_t            Color = 0;      // filled by GetEvents from category cfg
};
```

`MemoryOperationInfo` (`DebugTypes.h`):
```
struct MemoryOperationInfo { uint32_t Address; int32_t Value;
    MemoryOperationType Type; MemoryType MemType; };
```

`EventFlags` (bit flags, `BaseEventManager.h`): `PreviousFrame`,
`RegFirstWrite`, `RegSecondWrite`, `WithTargetMemory`, `SmsVdpPaletteWrite`,
`ReadWriteOp`. SNES sets `ReadWriteOp` on the memory-op `AddEvent` overload.

`DebugEventType` enum (`DebugTypes.h`, order = value):
`Register, Nmi, Irq, Breakpoint, BgColorChange, SpriteZeroHit, DmcDmaRead,
DmaRead`. SNES only emits `Register`, `Nmi`, `Irq`, `Breakpoint`.

### A.2 When/where an event is recorded (`SnesEventManager.cpp:30-64`)

Two overloads append to `_debugEvents`:

- `AddEvent(type, MemoryOperationInfo& op, breakpointId=-1)` — the register
  access path (`SnesEventManager.cpp:30`):
  - `Flags = ReadWriteOp`, `Operation = op`.
  - `Scanline = ppu->GetScanline()`, `Cycle = memoryManager->GetHClock()`.
  - If op is `DmaRead`/`DmaWrite`: `DmaChannel = dma->GetActiveChannel()`,
    `DmaChannelInfo = dma->GetChannelConfig(DmaChannel & 7)`; else `-1`.
  - `ProgramCounter = debugger->GetProgramCounter(Snes, true)`.
- `AddEvent(type)` — the NMI/IRQ path (`:52`): only `Type`, `Scanline`,
  `Cycle`, `ProgramCounter = (K<<16)|PC`, no operation.

The Debugger calls these as it processes each CPU/DMA memory access and each
NMI/IRQ. Every event of the live frame accumulates in `_debugEvents`.

### A.3 Per-frame double buffer / snapshot lifecycle (`BaseEventManager.cpp`)

- `ClearFrameEvents()` (`:78`): `_prevDebugEvents = _debugEvents;
  _debugEvents.clear();`  — called at frame end. So **two** rolling frames
  are kept: the just-finished (`_prevDebugEvents`) and the building one.
- `TakeEventSnapshot(forAutoRefresh)` (`SnesEventManager.cpp:158`): the
  debugger break-point. Under a `DebugBreakHelper` + lock it copies:
  - `_snapshotCurrentFrame = _debugEvents`, `_snapshotPrevFrame = _prevDebugEvents`.
  - `_snapshotScanline`, `_snapshotCycle` = current PPU pos.
  - `_overscanMode`, `_useHighResOutput` flags, and the PPU framebuffer (see C).
  - returns `_scanlineCount = ppu->GetVblankEndScanline()+1`.
- `FilterEvents()` (`BaseEventManager.cpp:4`): rebuilds `_sentEvents` (the
  drawable/visible set) from the snapshots — see E for the prev-frame logic.

So: live capture → `_debugEvents`; rotate to `_prevDebugEvents` each frame;
on a viewer refresh, snapshot both + framebuffer; `FilterEvents` produces the
visible `_sentEvents`; the GUI then asks for the overlay and/or the list.

---

## B) Category list, default colors, address→category mapping

### B.1 Config struct (`SnesEventManager.h:15`, `SnesEventViewerConfig`)

Each category is an `EventViewerCategoryCfg { uint32_t Color; bool Visible; }`
(C# default `Visible = true`, `EventViewerCategoryCfg.cs`). Members:

PPU writes (sub-categorised), PPU reads, APU R/W, CPU R/W, WRAM R/W, NMI, IRQ,
MarkedBreakpoints — plus `bool ShowPreviousFrameEvents` and
`uint8_t ShowDmaChannels[8]`.

### B.2 Default colors (`UI/Config/Debugger/SnesEventViewerConfig.cs`)

All default `Visible = true`. `ShowPreviousFrameEvents = true`,
`ShowDmaChannel0..7 = true`. Colors are `Color.FromRgb(R,G,B)`:

| Category (config field)        | Hex     | RGB             |
|--------------------------------|---------|-----------------|
| PpuRegisterCgramWrites         | #C92929 | (201, 41, 41)   |
| PpuRegisterVramWrites          | #B47ADA | (180, 122, 218) |
| PpuRegisterOamWrites           | #53D744 | (83, 215, 68)   |
| PpuRegisterMode7Writes         | #FE787B | (254, 120, 123) |
| PpuRegisterBgOptionWrites      | #BF8020 | (191, 128, 32)  |
| PpuRegisterBgScrollWrites      | #4A7CD9 | (74, 124, 217)  |
| PpuRegisterWindowWrites        | #E251F7 | (226, 81, 247)  |
| PpuRegisterOtherWrites         | #D1DD42 | (209, 221, 66)  |
| PpuRegisterReads               | #007597 | (0, 117, 151)   |
| CpuRegisterWrites              | #FF5E5E | (255, 94, 94)   |
| CpuRegisterReads               | #1898E4 | (24, 152, 228)  |
| ApuRegisterWrites              | #9F93C6 | (159, 147, 198) |
| ApuRegisterReads               | #F9FEAC | (249, 254, 172) |
| WorkRamRegisterWrites          | #2EFF28 | (46, 255, 40)   |
| WorkRamRegisterReads           | #8E33FF | (142, 51, 255)  |
| Nmi                            | #ABADAC | (171, 173, 172) |
| Irq                            | #C4F47A | (196, 244, 122) |
| MarkedBreakpoints              | #1898E4 | (24, 152, 228)  |

(UI groups these as: "PPU Register Writes" = the 8 PpuRegister*Writes; "Other
events" = PpuRegisterReads, APU/CPU/WRAM R/W, IRQ, NMI, MarkedBreakpoints;
"DMA Filters" = ShowDmaChannel0..7.)

### B.3 Address → category (`SnesEventManager.cpp:103-150`, `GetEventConfig`)

DMA gating first (`:106`): if a `Register` event is a DMA op and
`ShowDmaChannels[DmaChannel & 7] == 0`, return empty `{}` (hidden).

Then by `evt.Type`:
- `Breakpoint` → MarkedBreakpoints; `Irq` → Irq; `Nmi` → Nmi.
- `Register`: let `reg = Operation.Address & 0xFFFF`,
  `isWrite = (Type==Write || Type==DmaWrite)`.

  If `reg <= 0x213F` (PPU):
  - if `isWrite`, sub-categorise by `reg`:
    | reg range        | category                  |
    |------------------|---------------------------|
    | 0x2101 – 0x2104  | PpuRegisterOamWrites      |
    | 0x2105 – 0x210C  | PpuRegisterBgOptionWrites |
    | 0x210D – 0x2114  | PpuRegisterBgScrollWrites |
    | 0x2115 – 0x2119  | PpuRegisterVramWrites     |
    | 0x211A – 0x2120  | PpuRegisterMode7Writes    |
    | 0x2121 – 0x2122  | PpuRegisterCgramWrites    |
    | 0x2123 – 0x212B  | PpuRegisterWindowWrites   |
    | else (incl. 0x2100, 0x212C–0x213F) | PpuRegisterOtherWrites |
  - else (read) → PpuRegisterReads.

  Else if `reg <= 0x217F` → APU R/W (Apu*Writes / Apu*Reads).
  Else if `reg <= 0x2183` → WorkRam R/W (WorkRam*Writes / *Reads).
  Else if `reg >= 0x4000` → CPU R/W (Cpu*Writes / *Reads).
  Else → empty `{}` (e.g. 0x2184–0x3FFF).

  Note the OAM bucket is 0x2101–0x2104, so $2100 (INIDISP) lands in "Others".
  BG scroll bucket includes $210D–$2114 (the 8 H/VOFS regs). Mode-7 bucket is
  $211A–$2120 (M7SEL..M7Y). Window is $2123–$212B (W12SEL..WOBJLOG).

---

## C) Framebuffer overlay rendering

### C.1 Coordinate system & buffer size

- `ScanlineWidth = 1364/2 = 682` (`SnesEventManager.h:45`). Display buffer is
  **682 × (`_scanlineCount`×2)** ARGB (`GetDisplayBufferSize`,
  `SnesEventManager.cpp:188`). `_scanlineCount` = vblankEnd+1 (≈262 NTSC).
- Mapping scanline×cycle → pixel (`ConvertScanlineCycleToRowColumn`, `:152`):
  `y *= 2; x /= 2;`  (Y doubled → 2 px per scanline; X = H-clock/2.)
  Inverse, in `GetEvent` (`:66`): `x *= 2; y /= 2;`

### C.2 `GetDisplayBuffer` pipeline (`BaseEventManager.cpp:83`)

1. Clear whole buffer to `0xFF555555` (dark gray border).
2. `DrawScreen(buffer)` — composite the captured PPU image (C.3).
3. `DrawEvents(buffer, size)` — current-scanline line, event dots, cursor (C.4).

### C.3 `DrawScreen` — the game image under the dots (`SnesEventManager.cpp:196`)

The snapshot's `_ppuBuffer` (RGB555) is blitted into the ARGB display buffer:
- src skips 7 top blank lines when overscan off:
  `src = _ppuBuffer + (overscan ? 0 : (hires ? 512*14 : 256*7))`.
- height `len = overscan ? 239*2 : 224*2`; loop `x` over 0..511.
- per pixel: `srcOffset = hires ? ((y<<9)|x) : (((y>>1)<<8)|(x>>1))`
  (lo-res samples each src px into a 2×2 block).
- dest: `buffer[(y+2)*682 + x + 44] = Rgb555ToArgb(src[srcOffset])`.
  i.e. the 512-wide game image is centred with a +2-row, +44-col offset
  inside the 682-wide buffer.

Snapshot framebuffer choice (`TakeEventSnapshot`, `:169`): if past NMI scanline
or scanline 0, copy the whole live screen; otherwise copy the live screen up to
the current scanline and the **previous** frame's screen below it (so the image
is coherent mid-frame). `_ppuBuffer` sized `512*478` uint16.

### C.4 `DrawEvents` (`BaseEventManager.cpp:127`)

1. If not auto-refresh: `DrawLine(buffer, size, 0xFFFFFF55, _snapshotScanline)`
   — a translucent-yellow current-scanline marker (fills 2 rows,
   `DrawLine` `:103`).
2. `FilterEvents()` then draw `_sentEvents` **twice**:
   - pass 1: `DrawEvent(evt, drawBackground=true, …)` — the dimmed halo.
   - pass 2: `DrawEvent(evt, drawBackground=false, …)` — the bright core.
   (Two passes so cores always sit on top of neighbours' halos.)
3. If not auto-refresh, draw the cursor at the snapshot position:
   `DrawDot(x, y, 0xFF990099, true)` then `DrawDot(x, y, 0xFFFF00FF, false)`
   (magenta). `y = _snapshotScanline + _snapshotScanlineOffset`.

`DrawEvent` (`:115`): look up category color, convert (scanline,cycle)→(x,y),
`DrawDot`.

### C.5 `DrawDot` — the dot shape (`BaseEventManager.cpp:32`)

```
if(drawBackground) color = 0xFF000000 | ((color>>1) & 0x7F7F7F); // 50% dim
else               color |= 0xFF000000;                          // opaque core
iMin=jMin = drawBackground ? -2 : 0;
iMax=jMax = drawBackground ? +3 : +1;
for i in iMin..=iMax: for j in jMin..=jMax:
   skip if x+j >= width; pos=(y+i)*width + x+j; bounds-check; buffer[pos]=color;
```

So a **core** = 2×2 px solid; a **halo/background** = 6×6 px (-2..+3) at half
brightness drawn underneath. Net visible marker ≈ 6×6 dimmed square with a 2×2
bright center.

### C.6 Hi-res / overscan handling

`_useHighResOutput` (512-wide src) and `_overscanMode` (239 vs 224 visible
lines) drive the src offsets/strides in C.3 and the top-line skip. Display
buffer width stays 682; height scales with `_scanlineCount*2`.

---

## D) Register-name decode

Lives in **`UI/Debugger/Labels/DefaultLabelHelper.cs`**, method
`SetSnesDefaultLabels()` — a table of `LabelManager.SetLabel(addr, …, name, …)`.
These default labels are what the list view and tooltips show (e.g. "BG1SC").
luna would replicate this as a `addr → &'static str` table.

Representative $2100–$210F:

| Addr | Name | Addr | Name |
|------|------|------|------|
|2100|INIDISP|2108|BG2SC|
|2101|OBSEL |2109|BG3SC|
|2102|OAMADDL|210A|BG4SC|
|2103|OAMADDH|210B|BG12NBA|
|2104|OAMDATA|210C|BG34NBA|
|2105|BGMODE|210D|BG1HOFS|
|2106|MOSAIC|210E|BG1VOFS|
|2107|BG1SC |210F|BG2HOFS|

Representative $4200–$420D:

| Addr | Name | Addr | Name |
|------|------|------|------|
|4200|NMITIMEN|4207|HTIMEL|
|4201|WRIO    |4208|HTIMEH|
|4202|WRMPYA  |4209|VTIMEL|
|4203|WRMPYB  |420A|VTIMEH|
|4204|WRDIVL  |420B|MDMAEN|
|4205|WRDIVH  |420C|HDMAEN|
|4206|WRDIVB  |420D|MEMSEL|

(Full table continues to $213F PPU regs and $4210–$437F CPU/DMA regs in the
same method — port the whole `SetSnesDefaultLabels` table for completeness.)

---

## E) List view, toggles, DMA filters

### E.1 List columns

The flat list is the `_sentEvents` set retrieved via `GetEvents`
(`BaseEventManager.cpp:60`), which stamps each row's `Color` from its category.
Per-row data available → columns: **Scanline** (`evt.Scanline`), **Cycle**
(`evt.Cycle`, the H-clock), **PC** (`evt.ProgramCounter`, K:PC), **Type**
(category/register name via D), **Address** (`evt.Operation.Address`), plus
Value and the color swatch. (The C# `EventViewerListView` viewmodel binds these
fields; the Core supplies them in `DebugEventInfo`.)

### E.2 The two toggles

- **"Show previous frame events"** → `ShowPreviousFrameEvents` (default true).
  In `FilterEvents` (`BaseEventManager.cpp:9`): when true and not
  auto-refresh, previous-frame events that occur *after* the current cursor
  position are included and flagged `EventFlags::PreviousFrame`. The cursor key
  is `(snapshotScanline<<16)+snapshotCycle`; an event is kept if its key
  (`(Scanline+offset)<<16 + Cycle`) is **greater** (i.e. it happened later in
  the prev frame than where we are now in this frame), so the overlay shows a
  full frame's worth of events without duplicating the part already replaced by
  the current frame. Prev-frame dots are drawn with the same color (the
  PreviousFrame flag is available for the UI to dim/mark them if desired).
- **"Show list view"** — a pure UI toggle showing/hiding the list pane
  (no Core effect; the overlay always renders).

### E.3 DMA channel filters

`ShowDmaChannels[0..7]` (default all on). Enforced in `GetEventConfig`
(`:106`): a DMA-sourced `Register` event whose channel's flag is 0 returns the
empty config → `Visible=false` → excluded from both `_sentEvents` (list) and
the overlay. Channel = `evt.DmaChannel & 7`.

---

## F) Suggested staged port plan for luna

Order chosen so each stage is independently testable and matches luna's
API-first rule (everything flows through `luna-api`, the GUI consumes it).

**Stage 0 — leverage what exists.** luna already has a `mem-trace` carrying
`(scanline, dot, addr, value, op)` and luna-api/luna-gui debug panels (native
winit debug windows, fed by cheap `luna-api` accessors). The event capture is
essentially that trace plus NMI/IRQ markers — reuse its hook points rather than
adding new instrumentation.

**Stage 1 — core capture + categorise (in `luna-api`/`luna-core`).**
- Define `DebugEventType { Register, Nmi, Irq, Breakpoint }`, an event struct
  mirroring D.A.1 (scanline, cycle=H-clock, pc, address, value, op-type,
  dma_channel, flags), and a double-buffer (`cur`/`prev`) cleared per frame
  (B.A.3 `ClearFrameEvents`).
- Hook register accesses ($2100–$21FF, $4000+) and NMI/IRQ dispatch to push
  events. Cycle = current H-clock; scanline = current scanline.
- Port `GetEventConfig` categorisation **exactly** as B.3 (the `<= 0x213F`
  PPU sub-buckets are the load-bearing part — copy the ranges verbatim).
- Port the `SnesEventViewerConfig` defaults (B.2 colors, all Visible, DMA all
  on, ShowPreviousFrameEvents=true). Expose set/get on `luna-api::Emulator`.
- Add a `take_snapshot()` (B.A.3) returning the visible event list + the PPU
  framebuffer snapshot, behind `luna-api`.
- Unit-test categorisation: feed synthetic addresses across each boundary
  ($2100/$2101/$2104/$2105/$210C/$210D/$2114/$2115/$2119/$211A/$2120/$2121/
  $2122/$2123/$212B/$212C/$213F/$2140/$2180/$2183/$4200) and assert category.

**Stage 2 — register-name decode (core or api).**
- Port the `SetSnesDefaultLabels` SNES table (D) as a static `addr→name` map
  for $2100–$213F and $4200–$437F. Expose `register_name(addr) -> Option<&str>`
  via `luna-api`. Pure data; trivially unit-tested.

**Stage 3 — GUI framebuffer overlay (`luna-gui` debug window).**
- New debug window (like the existing native winit panels). Render the 682×(N*2)
  ARGB buffer: clear to `0xFF555555`, blit the snapshot PPU image with the +2
  row / +44 col centring and the lo-res 2×2 / hi-res mapping (C.3), then the
  current-scanline yellow line, the two-pass event dots (halo then core,
  `DrawDot` exactly per C.5: 6×6 dim under 2×2 bright), and the magenta cursor.
- Reuse luna's existing per-dot mapping knowledge (it already reasons in
  line+dot). Validate visually per the audible/visible-fixes rule.

**Stage 4 — GUI list + filters + toggles.**
- List pane with columns Scanline / Cycle / PC / Type(+reg name) / Address /
  Value (E.1), color swatch from category.
- "Show previous frame events" toggle wired to `ShowPreviousFrameEvents` and the
  `FilterEvents` prev-frame inclusion rule (E.2); "Show list view" pane toggle;
  8 DMA-channel checkboxes wired to `ShowDmaChannels[]` (E.3).
- Click-on-overlay → `GetEvent(y,x)` hit-test (C.1 inverse mapping + the
  two-pass ±2 / ±4..+6 proximity search, `SnesEventManager.cpp:66`) to select
  the corresponding list row.

**Pieces luna already has (don't rebuild):** the mem-trace (scanline+dot+
addr+value), the PPU framebuffer access via `luna-api` (`render_frame_rgba`),
the native debug-window infrastructure, H-clock/scanline counters. The genuinely
new work is the categorise table (B.3), the color/config struct (B.2), the
snapshot double-buffer (A.3), the overlay compositor (C), and the name table (D).
