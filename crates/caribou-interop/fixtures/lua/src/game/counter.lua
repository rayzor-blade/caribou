-- A class as Lua writes one, which Haxe and Wren use as a class of their
-- own: a table of functions that is its instances' metatable. `new`
-- constructs; a function declared with `:` is a method of an instance;
-- the fields `new` gives an instance are its fields, and the class's
-- other fields are statics. The LuaLS annotations are the types the
-- other languages see.

---@class Counter
---@field n integer the count
---@field label string
local Counter = {}
Counter.__index = Counter

---@type integer
Counter.LIMIT = 10

---@param start integer
---@return Counter
function Counter.new(start)
  return setmetatable({ n = start, label = "count" }, Counter)
end

-- Adds `by`, up to the limit.
---@param by integer
---@return integer
function Counter:bump(by)
  self.n = math.min(self.n + by, Counter.LIMIT)
  return self.n
end

-- The sum of what `f` gives for each of 1 to the count: `f` is the
-- caller's function, called from Lua.
---@param f fun(i: integer): integer
---@return integer
function Counter:sum(f)
  local total = 0
  for i = 1, self.n do total = total + f(i) end
  return total
end

-- A counter one past this one.
---@return Counter
function Counter:next()
  return Counter.new(self.n + 1)
end

-- The count and the label at once.
---@return integer count
---@return string label
function Counter:state()
  return self.n, self.label
end

-- The count a text names, or nil and why not, as Lua reports a failure.
---@param text string
---@return integer? count
---@return string? error
function Counter.parse(text)
  local n = tonumber(text)
  if n == nil then return nil, "not a number: " .. text end
  return n
end

-- The sum of the bytes of a buffer, read in place.
---@return integer
function Counter.checksum(bytes)
  local total = 0
  for i = 1, #bytes do total = total + string.byte(bytes, i) end
  return total
end

-- A Lua function, for the caller to call.
---@param n integer
---@return fun(m: integer): integer
function Counter.adder(n)
  return function(m) return n + m end
end

return { Counter = Counter }
