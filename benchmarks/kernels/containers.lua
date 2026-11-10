-- Kernel: the container closes — cons/head/tail/collect/first/max over
-- ints. Mirrors containers.ds exactly: a prepend ladder builds a
-- 15000-element table (each prepend copies the whole predecessor into
-- a fresh table), a head/tail drain walks it down one element per
-- iteration keeping a running max (tail copies the remainder each
-- step), 150 fresh copies re-read the full table, and the first
-- element pins the head. The nil answers map the ds checked read's
-- none side to -1. Indexes stay 0-based to mirror ds, converted at the
-- table library's 1-based edges.

local xs = {}
for i = 0, 14999 do
  local v = (i * 37) % 900 + 100
  local fresh = { v }
  local n = #xs
  for j = 1, n do
    fresh[j + 1] = xs[j]
  end
  xs = fresh
end

local m = 0
local rest = xs
local c = 0
while c < 15000 do
  local h = rest[1]
  if h == nil then
    h = -1
  end
  m = math.max(m, h)
  local nxt = {}
  local n = #rest
  for j = 2, n do
    nxt[j - 1] = rest[j]
  end
  rest = nxt
  c = c + 1
end

local total = 0
for _r = 0, 149 do
  local ys = {}
  local n = #xs
  for j = 1, n do
    ys[j] = xs[j]
  end
  total = total + #ys
end

local fv = xs[1]
if fv == nil then
  fv = -1
end

print(string.format("result: %d %d %d %d", #xs, m, total, fv))
