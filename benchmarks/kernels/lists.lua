-- Kernel: the list surface — table split by hand (string.find scanning),
-- indexed gets, and repeated whole-table joins. Mirrors lists.ds
-- exactly: 20000 three-digit pieces built through concat (the
-- empty-string seed), one split, 120 checksum
-- rounds of indexed reads with tonumber parsing, then 32 table copies
-- into a fresh table each time and a full sweep of the 660000-element
-- result. Indexes stay 0-based to mirror ds, converted at the string
-- and table library's 1-based edges.

local s = ""
for gi = 0, 19 do
  local piece = ""
  for k = 0, 999 do
    local v = (gi * 1000 + k) * 37 % 900 + 100
    piece = piece .. string.format("%d", v) .. ";"
  end
  s = s .. piece
end
s = string.sub(s, 1, 20000 * 4 - 1)

local parts = {}
local np = 0
local pos = 1
while true do
  local sep = string.find(s, ";", pos, true)
  if sep == nil then
    np = np + 1
    parts[np] = string.sub(s, pos)
    break
  end
  np = np + 1
  parts[np] = string.sub(s, pos, sep - 1)
  pos = sep + 1
end

local sum = 0
for _p = 0, 119 do
  for i = 0, 19999 do
    sum = sum + tonumber(parts[i + 1])
  end
end

local acc = parts
for _c = 0, 31 do
  local joined = {}
  local na = #acc
  for i = 1, na do
    joined[i] = acc[i]
  end
  for i = 1, 20000 do
    joined[na + i] = parts[i]
  end
  acc = joined
end

local total = 0
for j = 0, 20000 * 33 - 1 do
  total = total + tonumber(acc[j + 1])
end

print(string.format("result: %d %d %d", sum, total, #acc))
