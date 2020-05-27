local profiler = require("liblprofile_hook").Profiler()

local g

local function f(x)
  if x > 2 then
    return f(x - 1) + g(x - 1)
  else
    return x
  end
end

function g(x)
  if x > 2 then
    return g(x - 1) + f(x - 1)
  else
    return x + 1
  end
end

local result = profiler(function()
  for i = 1, 10e6, 1 do
    local a = 4 + 4
  end

  print(f(20))
end)

table.sort(result, function(lhs, rhs)
  return lhs.totalTime < rhs.totalTime
end)

for _, v in ipairs(result) do
  print(v.name, v.calls, v.totalTime, v.totalSelfTime)
end

print("total time:", result.totalTime)
