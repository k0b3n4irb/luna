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
