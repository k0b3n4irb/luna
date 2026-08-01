-- mesen-dsp1-port-trace.lua — Mesen2 reference capture for luna's DSP-1
-- port-level differential (docs/accuracy_scorecard.md open item #5).
--
-- WHAT IT DOES
--   Logs every S-CPU access to the DSP-1's DR/SR window on the HiROM 1K
--   board (banks $00-$1F / $80-$9F, offsets $6000-$7FFF; bit 12 selects
--   $6xxx = DR, $7xxx = SR) as CSV: master_clock, 24-bit addr, R/W, value.
--   The DR byte stream (command bytes written, result bytes read) is the
--   DSP-1's complete observable behaviour — diffing it against luna's
--     luna state <rom> ... (or the dsp1_port_differential test, which
--     captures the same window via Emulator::enable_mem_trace)
--   validates the uPD7725 core end-to-end. SR reads are captured too but
--   the diff should ignore their COUNT (RQM polling is timing-sensitive);
--   the DR sequence is timing-insensitive protocol data.
--
-- HOW TO RUN
--   ~/bin/Mesen --testRunner tools/mesen-dsp1-port-trace.lua \
--       "tests/roms/Super Mario Kart (USA).sfc" -novideo -noaudio
--   (writes /tmp/mesen_dsp1_port.csv; needs the dsp1.rom firmware in
--   Mesen's Firmware folder). DSP1_STOP_FRAME overrides the 3600-frame
--   (~60 s — title + demo race) capture window.
--
-- NOTES (Mesen2 specifics — see tools/mesen-irq-trace.lua)
--   * emu.read / callback values are SIGNED — mask & 0xFF.
--   * getState() inside a memory callback: only top-level fields valid.
--   * Callbacks are registered per bank so ROM fetches at $8000+ of the
--     same banks don't flood the log.

local out = io.open("/tmp/mesen_dsp1_port.csv", "w")
out:write("master_clock,addr,kind,value\n")

local function log(addr, value, kind)
  local st = emu.getState()
  out:write(string.format("%d,$%06X,%s,$%02X\n", st.masterClock, addr & 0xFFFFFF, kind, value & 0xFF))
end

local function onRead(addr, value)  log(addr, value, "R") end
local function onWrite(addr, value) log(addr, value, "W") end

local M = emu.memType.snesMemory
local C = emu.cpuType.snes

local function hookBank(bank)
  local lo = (bank << 16) | 0x6000
  local hi = (bank << 16) | 0x7FFF
  emu.addMemoryCallback(onRead,  emu.callbackType.read,  lo, hi, M, C)
  emu.addMemoryCallback(onWrite, emu.callbackType.write, lo, hi, M, C)
end

for bank = 0x00, 0x1F do hookBank(bank) end
for bank = 0x80, 0x9F do hookBank(bank) end

local STOP_FRAME = tonumber(os.getenv("DSP1_STOP_FRAME") or "3600")
local frame = 0
emu.addEventCallback(function()
  out:flush()
  frame = frame + 1
  if frame >= STOP_FRAME then
    out:close()
    emu.stop(0)
  end
end, emu.eventType.endFrame)
