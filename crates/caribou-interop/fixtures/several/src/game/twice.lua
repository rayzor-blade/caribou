-- A Lua function with two results, which a Python module unpacks.
local M = {}

-- The quotient and the remainder at once.
---@param a integer
---@param b integer
---@return integer quotient
---@return integer remainder
function M.qr(a, b)
  return a // b, a % b
end

return M
