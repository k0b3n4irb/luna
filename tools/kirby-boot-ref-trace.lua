-- kirby-boot-ref-trace.lua — Mesen2 reference-trace capture for the
-- luna "Kirby Super Star boots to a crash" investigation.
--
-- WHAT THIS ANSWERS
--   luna's S-CPU boot never writes the WRAM stub at $00:000E, so its
--   `JMP $000E` executes a BRK ($00) and the CPU runs away (see the
--   `project_smrpg_sa1_deadlock` notes / docs/sa1_status.md). On real
--   hardware *something* fills WRAM $0000-$001F before that jump. This
--   script logs, on a KNOWN-GOOD emulator:
--     (1) every write to WRAM $0000-$001F  -> WHAT populates the stub
--         (the PC doing it, the value, and via which path), and
--     (2) a bounded S-CPU PC stream from reset -> so luna's --cpu-trace
--         PC stream can be diffed against it to find the FIRST divergent
--         branch (= where luna skips the stub setup).
--
-- HOW TO RUN (Mesen2, https://github.com/SourMesen/Mesen2)
--   1. Power on Kirby Super Star (USA).
--   2. Debug -> Script Window -> open this file -> Run.
--   3. Hard-reset the console (Power cycle) so capture starts at reset.
--   4. Let it run ~3-4 seconds (past the title-load), then stop.
--   5. Two files appear next to the Mesen2 working dir:
--        kirby_wram_stub_writes.log   (the key answer)
--        kirby_pc_trace.log           (for the luna diff)
--   6. Send both back (or just the first if it's small).
--
-- NOTE ON API: field names below match Mesen2's SNES Lua state
-- (state.cpu.k/pc/a/x/y/sp/d/dbr/ps). If your build differs, run
--   `for k,v in pairs(emu.getState().cpu) do emu.log(k) end`
-- once to list the actual field names and adjust `cpuStr()`.

local STUB_LO, STUB_HI = 0x0000, 0x001F   -- WRAM offsets to watch (the stub)
local PC_TRACE_MAX     = 20000            -- cap the PC stream (boot is short)

local wramFile = io.open("kirby_wram_stub_writes.log", "w")
local pcFile   = io.open("kirby_pc_trace.log", "w")
local pcCount  = 0
local done     = false

local function cpuState()
  return emu.getState().cpu
end

-- Full 24-bit program counter "BB:PPPP".
local function pcStr(c)
  return string.format("%02X:%04X", c.k or 0, c.pc or 0)
end

-- One CSV-ish line matching luna's --cpu-trace columns we care about:
-- pc,a,x,y,sp,p,db,dp  (mclk/frame/e omitted; we diff on the PC stream).
local function cpuStr(c)
  return string.format("$%s A=%04X X=%04X Y=%04X SP=%04X P=%02X DB=%02X DP=%04X",
    pcStr(c), c.a or 0, c.x or 0, c.y or 0, c.sp or 0,
    c.ps or 0, c.dbr or 0, c.d or 0)
end

-- (1) Watch WRAM $0000-$001F for writes from ANY path (CPU store, DMA,
--     block move). The work-RAM memory type catches the physical write
--     regardless of how it was addressed — exactly what we need since we
--     don't yet know the access path.
local function onStubWrite(address, value)
  if wramFile == nil then return end
  local c = cpuState()
  wramFile:write(string.format("WRAM[$%04X]=$%02X  by PC=$%s  | %s\n",
    address, value, pcStr(c), cpuStr(c)))
  wramFile:flush()
  emu.log(string.format("STUB WRITE: WRAM[$%04X]=$%02X by PC=$%s",
    address, value, pcStr(c)))
end

-- (2) Bounded PC stream from reset for the luna diff. Stops itself at
--     PC_TRACE_MAX or once the boot reaches `JMP $000E` ($00:8189),
--     whichever comes first — that JMP is the moment the stub must exist.
local function onExec(address, value)
  if done then return end
  local c = cpuState()
  if pcFile ~= nil then
    pcFile:write(cpuStr(c) .. "\n")
  end
  pcCount = pcCount + 1
  -- $00:8189 = JMP $000E (the boot's jump into the WRAM stub).
  if (c.k == 0x00 and c.pc == 0x8189) or pcCount >= PC_TRACE_MAX then
    done = true
    if pcFile  ~= nil then pcFile:flush();  pcFile:close();  pcFile  = nil end
    if wramFile ~= nil then wramFile:flush(); wramFile:close(); wramFile = nil end
    emu.log(string.format(
      "DONE: captured %d instrs; reached %s. Check kirby_wram_stub_writes.log",
      pcCount, pcStr(c)))
  end
end

-- Mesen2 callback registration. `emu.callbackType.write/exec` and
-- `emu.memType.snesWorkRam` / `emu.cpuType.snes` are the standard names;
-- on older Mesen2 builds these may be `emu.memCallbackType.*`.
emu.addMemoryCallback(onStubWrite, emu.callbackType.write,
  STUB_LO, STUB_HI, emu.cpuType.snes, emu.memType.snesWorkRam)

emu.addMemoryCallback(onExec, emu.callbackType.exec,
  0x0000, 0xFFFF, emu.cpuType.snes, emu.memType.snesMemory)

emu.log("kirby-boot-ref-trace: armed. Hard-reset the console now to capture from reset.")
