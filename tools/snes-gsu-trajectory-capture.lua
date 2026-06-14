-- snes-gsu-trajectory-capture.lua — Mesen2 reference-fixture capture for
-- luna's `gsu_trajectory_vs_mesen` oracle (crates/luna-bus/src/superfx.rs).
--
-- WHAT IT DOES
--   Captures ONE Super FX (GSU) GO-burst as the four fixture files luna's
--   trajectory harness consumes, so luna's GSU engine can be replayed
--   byte-for-byte against the reference (proves engine vs. integration):
--     mesen_gsu_full.csv       per-instruction (pc,opcode,sfr,sreg,dreg,pbr,r0..r15)
--     mesen_gsu_init.txt       GO-entry GSU control state (cbr/scbr/scmr/por/cfgr/…)
--     mesen_gsu_ram_start.bin  GSU work RAM at burst start
--     mesen_gsu_ram_stop1.bin  GSU work RAM at the burst's STOP (or row cap)
--
-- HOW TO RUN
--   ARM_FRAME=1500 ~/bin/Mesen --testRunner \
--     tools/snes-gsu-trajectory-capture.lua "<Super FX rom>"
--   Strip any 512-byte .smc copier header from the ROM you pass to luna's
--   harness (the harness reads the file raw; GSU PCs are headerless-space).
--   Then: LUNA_GSU_DIFF_DIR=/tmp LUNA_SF_ROM=<headerless.sfc> \
--           cargo test -p luna-bus gsu_trajectory_vs_mesen -- --nocapture
--
-- NOTES (Mesen2 specifics, learned the hard way)
--   * GSU register state lives under getState() keys "cart.coprocessor.*"
--     (r0..r15, sfr.*, cacheBase=cbr, screenBase=scbr, plotBpp→md, …).
--   * SFR bit layout written to match luna: Z1 CY2 S3 OV4 G5 R6 ALT1=8 ALT2=9 B12 IRQ15.
--   * exec callback `value` arg IS the opcode byte; getState() in the
--     callback is the PRE-execution state (matches the harness row layout).
--   * Mesen GSU work RAM is `emu.memType.gsuWorkRam` (32 KB for YI; the size
--     comes from the $FFBD expansion-RAM byte: 1024<<n, GSU default 64 KB).
local ARM_FRAME = tonumber(os.getenv("ARM_FRAME") or "1500")
local MAX_ROWS  = 20000

local frame = 0
local armed = false
local capturing = false
local sawStop = false
local saved = false
local rows = {}
local ramStart = nil

local function g(st, k) return st[k] or 0 end
local function b(st, k) if st[k] then return 1 else return 0 end end

local function dumpRam(path)
  local sz = emu.getMemorySize(emu.memType.gsuWorkRam)
  local f = io.open(path, "wb")
  for a = 0, sz - 1 do f:write(string.char(emu.read(a, emu.memType.gsuWorkRam, false) % 256)) end
  f:close()
  return sz
end

-- luna SFR bit layout: Z=1,CY=2,S=3,OV=4,G=5,R=6,ALT1=8,ALT2=9,B=12,IRQ=15
local function buildSfr(st)
  local s = 0
  if st["cart.coprocessor.sfr.zero"]            then s = s | (1<<1) end
  if st["cart.coprocessor.sfr.carry"]           then s = s | (1<<2) end
  if st["cart.coprocessor.sfr.sign"]            then s = s | (1<<3) end
  if st["cart.coprocessor.sfr.overflow"]        then s = s | (1<<4) end
  if st["cart.coprocessor.sfr.running"]         then s = s | (1<<5) end
  if st["cart.coprocessor.sfr.romReadPending"]  then s = s | (1<<6) end
  if st["cart.coprocessor.sfr.alt1"]            then s = s | (1<<8) end
  if st["cart.coprocessor.sfr.alt2"]            then s = s | (1<<9) end
  if st["cart.coprocessor.sfr.prefix"]          then s = s | (1<<12) end
  if st["cart.coprocessor.sfr.irq"]             then s = s | (1<<15) end
  return s
end

local function gsuExec(addr, opcode)
  if not capturing or saved then return end
  local st = emu.getState()
  if ramStart == nil then
    -- first captured instruction: snapshot RAM + init state (pre-execution)
    dumpRam("/tmp/mesen_gsu_ram_start.bin")
    ramStart = true
    local md_from_bpp = { [2]=0, [4]=1, [8]=3 }
    local por = b(st,"cart.coprocessor.plotTransparent")*1
              + b(st,"cart.coprocessor.plotDither")*2
              + b(st,"cart.coprocessor.colorHighNibble")*4
              + b(st,"cart.coprocessor.colorFreezeHigh")*8
              + b(st,"cart.coprocessor.objMode")*16
    local cfgr = b(st,"cart.coprocessor.irqDisabled")*128
               + b(st,"cart.coprocessor.highSpeedMode")*32
    local init = io.open("/tmp/mesen_gsu_init.txt", "w")
    init:write("ramSize="  .. emu.getMemorySize(emu.memType.gsuWorkRam) .. "\n")
    init:write("cbr="      .. g(st,"cart.coprocessor.cacheBase") .. "\n")
    init:write("scbr="     .. g(st,"cart.coprocessor.screenBase") .. "\n")
    init:write("ht="       .. g(st,"cart.coprocessor.screenHeight") .. "\n")
    init:write("md="       .. (md_from_bpp[g(st,"cart.coprocessor.plotBpp")] or 0) .. "\n")
    init:write("ron="      .. b(st,"cart.coprocessor.gsuRomAccess") .. "\n")
    init:write("ran="      .. b(st,"cart.coprocessor.gsuRamAccess") .. "\n")
    init:write("colr="     .. g(st,"cart.coprocessor.colorReg") .. "\n")
    init:write("por="      .. por .. "\n")
    init:write("cfgr="     .. cfgr .. "\n")
    init:write("clsr="     .. b(st,"cart.coprocessor.clockSelect") .. "\n")
    init:write("rombr="    .. g(st,"cart.coprocessor.romBank") .. "\n")
    init:write("rambr="    .. g(st,"cart.coprocessor.ramBank") .. "\n")
    init:write("romdr="    .. g(st,"cart.coprocessor.romReadBuffer") .. "\n")
    init:write("ramaddr="  .. g(st,"cart.coprocessor.ramAddress") .. "\n")
    init:write("ramar="    .. g(st,"cart.coprocessor.ramWriteAddress") .. "\n")
    init:write("ramdr="    .. g(st,"cart.coprocessor.ramWriteValue") .. "\n")
    init:close()
  end
  -- row: seq,pc,opcode,sfr,sreg,dreg,pbr, _, _, r0..r15  (>=25 cols)
  local r = {}
  for i = 0, 15 do r[#r+1] = tostring(g(st, "cart.coprocessor.r" .. i)) end
  rows[#rows+1] = table.concat({
    #rows,
    g(st,"cart.coprocessor.r15"),   -- pc (R15 = address being executed)
    opcode,
    buildSfr(st),
    g(st,"cart.coprocessor.srcReg"),
    g(st,"cart.coprocessor.destReg"),
    g(st,"cart.coprocessor.programBank"),
    0, 0,
    table.concat(r, ",")
  }, ",")
  if opcode == 0 then sawStop = true end          -- STOP opcode
  if sawStop or #rows >= MAX_ROWS then
    capturing = false                              -- stop logging; save next frame
  end
end

emu.addEventCallback(function()
  frame = frame + 1
  if frame >= ARM_FRAME and not armed then
    armed = true; capturing = true
    emu.addMemoryCallback(gsuExec, emu.callbackType.exec, 0, 0xFFFFFF,
                          emu.memType.gsuMemory, emu.cpuType.gsu)
  end
  if armed and not capturing and not saved and ramStart then
    saved = true
    dumpRam("/tmp/mesen_gsu_ram_stop1.bin")
    local csv = io.open("/tmp/mesen_gsu_full.csv", "w")
    csv:write("seq,pc,opcode,sfr,sreg,dreg,pbr,x,y,r0,r1,r2,r3,r4,r5,r6,r7,r8,r9,r10,r11,r12,r13,r14,r15\n")
    for _,line in ipairs(rows) do csv:write(line .. "\n") end
    csv:close()
    emu.log("captured " .. #rows .. " GSU rows (sawStop=" .. tostring(sawStop) .. ")")
    emu.exit()
  end
end, emu.eventType.endFrame)
