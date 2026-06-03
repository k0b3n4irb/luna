-- pitchmod-ref-check.lua — Mesen2 reference check for the luna
-- "PitchMod SPC700 crash" question.
--
-- THE QUESTION (binary, decisive):
--   In luna, PitchMod's SPC700 runs the music driver, then RUNS AWAY into a
--   data region and HALTS on a $FF (STOP) opcode at PC $FA42 — audio freezes
--   at ~56620 of 96000 samples (~1.77 s of ~3 s). This regressed when the
--   Phase-2 cycle fix (081e78d) corrected 9 SPC700 opcode cycle counts; a T0
--   ($FD, reload=1) read lands on the other side of a 128-cycle boundary.
--   luna's cycles (Tom-Harte) and timer model (ares) both check out, so the
--   open question is whether luna has a hidden bug or PitchMod is a knife-edge
--   timing ROM whose golden is just stale.
--
--   A KNOWN-ACCURATE emulator settles it:
--     - If its SPC700 ALSO halts (STOP/SLEEP) and the audio cuts out ~1.7 s
--       -> PitchMod is genuinely fragile; luna is correct; ignore the golden.
--     - If the SPC700 NEVER halts and audio plays the full ~3 s
--       -> luna has a real bug; REOPEN (don't ignore).
--
-- HOW TO RUN (Mesen2, https://github.com/SourMesen/Mesen2)
--   1. Load PitchMod.sfc (the same ROM luna tests:
--      <corpus>/SPC700/PitchMod/PitchMod.sfc). Headerless homebrew — if Mesen2
--      mis-detects the mapper, force LoROM. Region doesn't matter (the SPC
--      runs on its own clock).
--   2. Debug -> Script Window -> open this file -> Run.
--   3. Hard-reset, let it run ~5 seconds, then Stop.
--   4. Read pitchmod_ref.log (next to Mesen2's working dir) and/or the script
--      log: it prints whether the SPC700 HALTED and at what PC, plus a bounded
--      SPC PC stream for an optional diff against luna.
--
-- NOTE: field/callback names are Mesen2's SPC (sound CPU) Lua API. If a name
-- differs in your build, run once:  for k,_ in pairs(emu.getState().spc) do emu.log(k) end

local pcLog   = io.open("pitchmod_ref.log", "w")
local count   = 0
local halted  = false
local MAXLOG  = 40000   -- bounded PC stream for the diff

local function spc()  return emu.getState().spc  end

-- (1) Detect a HALT: the SPC executing $FF (STOP) or $EF (SLEEP). A working
--     music driver NEVER does this. This is the decisive signal.
local function onExec(address, value)
  if halted then return end
  local c = spc()
  count = count + 1
  if pcLog and count <= MAXLOG then
    pcLog:write(string.format("%04X:%02X A=%02X P=%02X\n", c.pc, value, c.a, c.ps))
  end
  -- `value` is the byte being executed (opcode) at this PC.
  if value == 0xFF or value == 0xEF then
    halted = true
    local what = (value == 0xFF) and "STOP ($FF)" or "SLEEP ($EF)"
    emu.log(string.format(
      ">>> SPC700 HALTED on %s at PC=$%04X after %d instrs <<<", what, c.pc, count))
    emu.log(">>> VERDICT: matches luna -> PitchMod is fragile, luna is correct, ignore the golden.")
    if pcLog then pcLog:write(string.format("HALT %s at $%04X count=%d\n", what, c.pc, count)) ; pcLog:flush() end
  end
  -- Also flag the data region luna crashes into ($FA00-$FAFF), in case this
  -- build's STOP byte differs.
  if not halted and c.pc >= 0xFA00 and c.pc <= 0xFAFF then
    emu.log(string.format("note: SPC PC in data region $%04X at instr %d (luna crashes here)", c.pc, count))
  end
end

-- Periodic heartbeat: if the SPC keeps running past ~2.5 s with no halt, that
-- is the "luna bug" verdict.
local frames = 0
local function onFrame()
  frames = frames + 1
  if frames == 180 and not halted then   -- ~3 s NTSC
    emu.log(string.format(
      ">>> ~3 s elapsed, SPC700 still running (%d instrs), NO halt <<<", count))
    emu.log(">>> VERDICT: luna diverges -> PitchMod is NOT fragile here, REOPEN as a luna bug.")
    if pcLog then pcLog:flush() end
  end
end

emu.addMemoryCallback(onExec, emu.callbackType.exec, 0x0000, 0xFFFF, emu.cpuType.spc)
emu.addEventCallback(onFrame, emu.eventType.startFrame)
emu.log("pitchmod-ref-check: armed. Hard-reset and let it run ~5 s, then read the log/verdict.")
