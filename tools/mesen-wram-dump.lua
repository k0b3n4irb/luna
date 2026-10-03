-- mesen-wram-dump.lua — Mesen2 raw WRAM dump at chosen frames, the
-- byte-level follow-up to the per-frame page-hash differential
-- (tools/snes-wram-perframe-hash.lua + tools/diff-wram-hashes.py): once
-- those name the first diverging frame, dump the full 128 KiB there and
-- compare byte by byte with luna's dump of the same frame.
--
-- Edit DUMPS below (frame -> output path; frames count EndFrame events
-- from 1, like the hash scripts) and the stop frame in onEnd, which is
-- hard-coded to 24. As committed it dumps frames 23 and 24, the pair
-- used in docs/archive/smrpg_intro_sa1_divergence.md.
--
-- HOW TO RUN
--   ~/bin/Mesen --testRunner tools/mesen-wram-dump.lua "<rom>" -novideo -noaudio
--   luna side: ./target/release/luna wram-trace -c <frames> --dump-frame <N> \
--                --dump-out /tmp/luna_fN.bin "<rom>"
--   (luna's N is its PPU frame: apply the offset diff-wram-hashes.py printed)
--   then: cmp -l /tmp/luna_fN.bin /tmp/mesen_fN.bin
-- Nothing in the repo consumes the dumps; they are read by hand.
local WRAM = emu.memType.snesWorkRam
local frame = 0
local DUMPS = { [23]="/tmp/mesen_f23.bin", [24]="/tmp/mesen_f24.bin" }
local function dump(path)
  local t = {}
  for a = 0, 0x1FFFF do t[#t+1] = string.char(emu.read(a, WRAM, false) & 0xFF) end
  local f = assert(io.open(path, "wb")); f:write(table.concat(t)); f:close()
end
local function onEnd()
  frame = frame + 1
  if DUMPS[frame] then dump(DUMPS[frame]) end
  if frame >= 24 then emu.stop(0) end
end
emu.addEventCallback(onEnd, emu.eventType.endFrame)
