-- Kernel: the lambda combinators — map/filter/reduce chains over int
-- and string arrays. Mirrors combinators.ds exactly: the three
-- combinators hand-rolled (Lua ships none), a 10000-element int list
-- built by insert, 250 rounds of map (x*2), filter (x % 3 == 0),
-- reduce (sum), then a 1000-element string list and 40 rounds of map
-- (suffix concat), filter (# < 5), reduce (concat join), checksummed
-- by length. Indexes stay 0-based to mirror ds, converted at the table
-- library's 1-based edges.

local function map(xs, f)
  local out = {}
  for i = 1, #xs do
    out[i] = f(xs[i])
  end
  return out
end

local function filter(xs, f)
  local out = {}
  local n = 0
  for i = 1, #xs do
    if f(xs[i]) then
      n = n + 1
      out[n] = xs[i]
    end
  end
  return out
end

local function reduce(xs, init, f)
  local acc = init
  for i = 1, #xs do
    acc = f(acc, xs[i])
  end
  return acc
end

local xs = {}
for i = 0, 9999 do
  xs[#xs + 1] = (i * 37) % 900 + 100
end

local sum = 0
for _r = 0, 249 do
  local doubled = map(xs, function(x) return x * 2 end)
  local kept = filter(doubled, function(x) return x % 3 == 0 end)
  sum = sum + reduce(kept, 0, function(a, b) return a + b end)
end

local ys = {}
for j = 0, 999 do
  ys[#ys + 1] = string.format("w%d", j % 40)
end

local lensum = 0
for _q = 0, 39 do
  local banged = map(ys, function(w) return w .. "!" end)
  local short = filter(banged, function(w) return #w < 5 end)
  local joined = reduce(short, "", function(a, b) return a .. b end)
  lensum = lensum + #joined
end

print(string.format("result: %d %d %d %d", sum, #xs, lensum, #ys))
