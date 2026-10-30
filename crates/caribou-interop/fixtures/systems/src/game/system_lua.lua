-- A Lua system: counts its steps in byte 0, and writes the step's time as
-- an f32 at byte 4.
local Systems = require("game.Systems").Systems

local M = {}

function M.install()
  Systems.register(function(column, dt)
    column:setU8(0, column:getU8(0) + 1)
    column:setF32(4, dt)
  end)
end

function M.installFailing()
  Systems.registerFailing(function(column, dt)
    error("lua failed")
  end)
end

function M.installFailingDynamic()
  Systems.registerFailingDynamic(function(column, dt)
    error("lua dynamic failed")
  end)
end

return M
