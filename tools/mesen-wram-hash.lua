-- mesen-smrpg-wram-hash.lua
-- Mesen2 reference generator for luna's `wram-trace` differential.
--
-- Emits, once per frame (at EndFrame = end of VBlank, matching luna's
-- frame-boundary sample), an FNV-1a 64-bit hash of each 4 KiB WRAM page
-- (32 pages over the 128 KiB SNES WRAM), byte-exact to luna-api
-- `wram_page_hashes`. The first frame whose hashes differ from luna's
-- pins the first real state divergence (no input => one game-frame per
-- NMI in both emulators).
--
-- USAGE (headless, fastest):
--   <mesen-binary> --testrunner "Super Mario RPG ... .sfc" tools/mesen-smrpg-wram-hash.lua
-- or load it via Debug > Script Window in the GUI and let it run.
-- Output: /tmp/mesen_wram.txt  (format: "<frame> <h0> ... <h31>")
--
-- Run with NO controller input (we are diffing the no-input intro).

local OUT = "/tmp/mesen_wram.txt"
local MAX_FRAMES = 2200
local PAGE = 4096
local NPAGES = 0x20000 // PAGE  -- 32

-- FNV-1a 64-bit constants (identical to luna).
local FNV_OFFSET = 0xcbf29ce484222325
local FNV_PRIME  = 0x100000001b3

local WRAM = emu.memType.snesWorkRam
local file = assert(io.open(OUT, "w"))
local frame = 0

local function onEndFrame()
  frame = frame + 1
  local parts = { tostring(frame) }
  local addr = 0
  for _ = 1, NPAGES do
    local h = FNV_OFFSET
    local pend = addr + PAGE
    while addr < pend do
      -- readWord is little-endian: low byte = WRAM[addr], high = WRAM[addr+1],
      -- so feeding low-then-high matches luna's linear byte order exactly.
      local w = emu.readWord(addr, WRAM, false)
      h = (h ~ (w & 0xFF)) * FNV_PRIME
      h = (h ~ ((w >> 8) & 0xFF)) * FNV_PRIME
      addr = addr + 2
    end
    parts[#parts + 1] = string.format("%016x", h)
  end
  file:write(table.concat(parts, " "), "\n")
  file:flush()  -- survive an early close
  if frame >= MAX_FRAMES then
    file:close()
    emu.removeEventCallback(onEndFrame, emu.eventType.endFrame)
    emu.displayMessage("luna-diff", "wrote " .. frame .. " frames to " .. OUT)
    emu.stop(0)
  end
end

emu.addEventCallback(onEndFrame, emu.eventType.endFrame)
emu.displayMessage("luna-diff", "hashing WRAM per frame -> " .. OUT)
