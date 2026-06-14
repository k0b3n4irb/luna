-- snes-wram-perframe-hash.lua — Mesen2 side of the NMI-aligned WRAM
-- differential. Produces the SAME per-frame WRAM page-hash table as
-- `luna wram-trace` (identical FNV-1a) so the first frame whose page hash
-- differs pins the first real game-state divergence.
--
-- HOW TO RUN
--   MAXF=700 ~/bin/Mesen --testRunner tools/snes-wram-perframe-hash.lua "<rom>"
--   luna side: ./target/release/luna wram-trace -c 700 --page-size 4096 \
--                --out /tmp/luna_wram.txt "<rom>"
--   then diff /tmp/mesen_wram.txt vs /tmp/luna_wram.txt (ignore page 0 =
--   volatile stack; ignore pages the game never clears = power-on RAM
--   confound — luna zeros WRAM, Mesen randomizes it).
--
-- CAVEAT: only confound-free when the game advances one logic step per NMI.
-- For multi-frame work whose per-frame progress depends on CPU/coproc speed
-- (e.g. an animated intro), per-frame state legitimately differs by phase.
-- Output: /tmp/mesen_wram.txt  "<frame> <h0> ... <h31>" (hex, 4KB pages).
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
