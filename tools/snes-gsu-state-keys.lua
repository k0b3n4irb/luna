-- snes-gsu-state-keys.lua — dump Mesen2 getState() keys for the Super FX
-- coprocessor (all "cart.coprocessor.*"), the reference for mapping Mesen
-- GSU state → luna's Registers in the trajectory-capture script.
--   ~/bin/Mesen --testRunner tools/snes-gsu-state-keys.lua "<Super FX rom>"
--   -> /tmp/yi_gsu_keys.txt
local done = false
local function onFrame()
  if done then return end
  done = true
  local st = emu.getState()
  local keys = {}
  for k,_ in pairs(st) do keys[#keys+1]=k end
  table.sort(keys)
  local out = io.open("/tmp/yi_gsu_keys.txt","w")
  for _,k in ipairs(keys) do
    local lk = string.lower(k)
    if lk:find("coprocessor") or lk:find("gsu") then
      out:write(k .. " = " .. tostring(st[k]) .. "\n")
    end
  end
  out:close()
  emu.exit()
end
-- wait a few frames so the GSU is initialized
local n=0
emu.addEventCallback(function() n=n+1; if n>=200 then onFrame() end end, emu.eventType.endFrame)
