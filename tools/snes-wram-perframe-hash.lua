-- snes-wram-perframe-hash.lua — Mesen2 side of the NMI-aligned WRAM
-- differential. Produces the SAME per-frame WRAM page-hash table as
-- `luna wram-trace` (identical FNV-1a) so the first frame whose page hash
-- differs pins the first real game-state divergence.
--
-- This is the script to use. tools/mesen-wram-hash.lua does the same job
-- (same event, pages, hash, output file and format) with a hard-coded
-- 2200-frame cap; it is kept only because archived investigations cite it.
--
-- HOW TO RUN
--   MAXF=700 ~/bin/Mesen --testRunner tools/snes-wram-perframe-hash.lua "<rom>" \
--     -novideo -noaudio
--   luna side: ./target/release/luna wram-trace -c 700 --page-size 4096 \
--                --out /tmp/luna_wram.txt "<rom>"
--   then: tools/diff-wram-hashes.py /tmp/luna_wram.txt /tmp/mesen_wram.txt
--   (it aligns the boot-frame offset and names the first diverging pages;
--   ignore page 0 = volatile stack, and pages the game never clears = the
--   power-on RAM confound: the two emulators do not boot on the same RAM).
--   Byte-level follow-up on the diverging frame: tools/mesen-wram-dump.lua
--   against `luna wram-trace --dump-frame N --dump-out <file>`.
--
-- CAVEAT: only confound-free when the game advances one logic step per NMI.
-- For multi-frame work whose per-frame progress depends on CPU/coproc speed
-- (e.g. an animated intro), per-frame state legitimately differs by phase.
-- Output: /tmp/mesen_wram.txt  "<frame> <h0> ... <h31>" (hex, 4KB pages).
-- First used in docs/archive/yoshis_island_text_barcode_investigation.md.
local PAGES = 32
local PAGE = 4096
local MAXF = tonumber(os.getenv("MAXF") or "700")
local PRIME = 0x100000001b3
local OFFSET = 0xcbf29ce484222325   -- 64-bit FNV offset basis (fits in Lua int64)

local frame = 0
local out = io.open("/tmp/mesen_wram.txt", "w")
local mt = emu.memType.snesWorkRam

local function onEndFrame()
  frame = frame + 1
  local line = { tostring(frame) }
  for p = 0, PAGES - 1 do
    local h = OFFSET
    local base = p * PAGE
    for i = 0, PAGE - 1 do
      local b = emu.read(base + i, mt, false) & 0xFF
      h = (h ~ b) * PRIME          -- int64 wraps mod 2^64, matching Rust wrapping_mul
    end
    line[#line + 1] = string.format("%016x", h)
  end
  out:write(table.concat(line, " ") .. "\n")
  out:flush()
  if frame >= MAXF then
    out:close()
    emu.log("wrote " .. frame .. " WRAM hash frames")
    emu.exit()
  end
end

emu.addEventCallback(onEndFrame, emu.eventType.endFrame)
