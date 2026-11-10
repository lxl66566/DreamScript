-- Kernel: the interim maps surface — set/get/len builder-call
-- sequences over string keys by content. Mirrors maps.ds exactly: one
-- table of 5000 distinct keys filled, 16 read rounds over all keys
-- (nil answers map to -1 like the ds checked read's none side), 2000
-- missing-key probes, a key-for-key overwrite pass observing the entry
-- count per set (Lua hashes have no #, so the count walks pairs), and
-- one final read round pinning the overwritten values.

local m = {}
for i = 0, 4999 do
  m[string.format("key-%d-%d", i % 7, i)] = (i * 37) % 900 + 100
end

local sum = 0
for _r = 0, 15 do
  for j = 0, 4999 do
    local v = m[string.format("key-%d-%d", j % 7, j)]
    if v == nil then
      v = -1
    end
    sum = sum + v
  end
end

local miss = 0
for q = 0, 1999 do
  local v = m[string.format("missing-%d", q)]
  if v == nil then
    v = -1
  end
  miss = miss + v
end

local n2 = 0
for k = 0, 4999 do
  m[string.format("key-%d-%d", k % 7, k)] = k % 100
  local n = 0
  for _ in pairs(m) do
    n = n + 1
  end
  n2 = n2 + n
end

local sum2 = 0
for w = 0, 4999 do
  local v = m[string.format("key-%d-%d", w % 7, w)]
  if v == nil then
    v = -1
  end
  sum2 = sum2 + v
end

local final_n = 0
for _ in pairs(m) do
  final_n = final_n + 1
end

print(string.format("result: %d %d %d %d %d", sum, miss, final_n, n2, sum2))
