-- Kernel: the record span channel. Mirrors records.ds exactly: a list
-- of N four-field tables (two ints, a float, a string) built by append,
-- then R read passes projecting the fields per element, a missing-index
-- probe over the same checked read (the fallback table answers the none
-- side), and the first element pinned. Scores cycle 0.25/0.75/1.25 over
-- an odd pass count so the printed float total is an odd fourth
-- (tostring's %.14g included). Indexes stay 0-based to mirror ds,
-- converted at the table library's 1-based edges.

local n = 20001
local xs = {}
for i = 0, n - 1 do
  local m = i % 3
  local score = 0.25
  if m == 1 then
    score = 0.75
  end
  if m == 2 then
    score = 1.25
  end
  local tv = (i % 40) + 10
  xs[i + 1] = { id = (i * 37) % 900 + 100, w = i % 7, score = score, name = "t" .. tv }
end

local blank = { id = -1, w = 0, score = 0.25, name = "t00" }
local sum_id = 0
local sum_w = 0
local lensum = 0
local total = 0.0
for _r = 1, 45 do
  for j = 0, n - 1 do
    local p = xs[j + 1]
    sum_id = sum_id + p.id
    sum_w = sum_w + p.w
    total = total + p.score
    lensum = lensum + #p.name
  end
end

local miss = 0
for q = 0, 1999 do
  local p = blank
  if n + q < #xs then
    p = xs[n + q + 1]
  end
  miss = miss + p.id
end

local hid = -1
if #xs > 0 then
  hid = xs[1].id
end

print(string.format("result: %d %d %d %d %d %s", sum_id, sum_w, lensum, miss, hid, tostring(total)))
