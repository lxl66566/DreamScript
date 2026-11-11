-- Kernel: build strings then reprocess them — concat, char scanning,
-- sub cuts, tonumber parsing. Mirrors strings.ds exactly: pieces are
-- three digits plus ';', accumulators seed with the empty string
-- literal. Indexes stay 0-based to mirror ds, converted at the string
-- library's 1-based edges.

local s = ""
for gi = 0, 24 do
  local piece = ""
  for k = 0, 999 do
    local v = (gi * 1000 + k) * 37 % 900 + 100
    piece = piece .. string.format("%d", v) .. ";"
  end
  s = s .. piece
end

local sum = 0
local start = 0
for j = 0, 99999 do
  if string.sub(s, j + 1, j + 1) == ";" then
    sum = sum + tonumber(string.sub(s, start + 1, j))
    start = j + 1
  end
end

print(string.format("result: %d %d", sum, 100000))
