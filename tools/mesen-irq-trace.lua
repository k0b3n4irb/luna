-- mesen-irq-trace.lua — Mesen2 reference capture for luna's P0 delivery-timing
-- differential harness (the cycle-accuracy roadmap, docs/roadmap_to_A.md).
--
-- WHAT IT DOES
--   Logs the interrupt-delivery-relevant S-CPU bus events with the master
--   clock, so they can be diffed against luna's
--     luna state <rom> --mem-trace luna_irq.csv --mem-trace-addr 4200:4211
--   (luna also emits synthetic `N`/`I` markers at the moment it raises the
--   NMI / H-V-IRQ line — the thing the deferred Phase-4 work changes).
--   Captured here:
--     $4200 NMITIMEN writes
--     $4210 RDNMI / $4211 TIMEUP reads
--     $FFEA/$FFEE (native) + $FFFA/$FFFE (emulation) NMI/IRQ vector fetches
--       — the actual delivery moment, bus-observable on both emulators.
--
-- HOW TO RUN
--   ~/bin/Mesen --testRunner tools/mesen-irq-trace.lua "<rom>" -novideo -noaudio
--   (writes /tmp/mesen_irq.csv). Then diff vs luna_irq.csv — compare the event
--   SEQUENCE and inter-event master-clock DELTAS, not absolute clocks (the two
--   emulators' clock origins differ; align on the first shared event).
--
-- NOTES (Mesen2 specifics)
--   * getState() inside a memory callback exposes only top-level fields —
--     masterClock is valid; cpu/ppu are nil, so derive dot/scanline from
--     masterClock (= what luna does: H = (mclk % 1364)/4, V = mclk/1364).
--   * emu.read returns SIGNED bytes — mask & 0xFF.
--   * the read/write callback `value` arg is the byte transferred.

local out = io.open("/tmp/mesen_irq.csv", "w")
out:write("master_clock,addr,kind,value\n")

local function log(addr, value, kind)
  local st = emu.getState()
  out:write(string.format("%d,$%04X,%s,$%02X\n", st.masterClock, addr & 0xFFFF, kind, value & 0xFF))
end

local function onRead(addr, value)  log(addr, value, "R") end
local function onWrite(addr, value) log(addr, value, "W") end
local function onVector(addr, value) log(addr, value, "V") end

local M = emu.memType.snesMemory
local C = emu.cpuType.snes

emu.addMemoryCallback(onWrite,  emu.callbackType.write, 0x4200, 0x4200, M, C)
emu.addMemoryCallback(onRead,   emu.callbackType.read,  0x4210, 0x4211, M, C)
emu.addMemoryCallback(onVector, emu.callbackType.read,  0xFFEA, 0xFFEF, M, C)
emu.addMemoryCallback(onVector, emu.callbackType.read,  0xFFFA, 0xFFFF, M, C)

emu.addEventCallback(function() out:flush() end, emu.eventType.endFrame)
