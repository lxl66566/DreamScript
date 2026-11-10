-- Kernel: build strings then reprocess them — concat, char scanning,
-- sub cuts, a char digit fold. Mirrors strings.ds exactly: pieces are
-- three digits plus ';', the fold's positional weights (100/10/1)
-- reconstruct the piece value tonumber parsing would answer (the digit
-- ladder replaces the parse native on the ds side), accumulators seed
-- with the empty string literal. Indexes stay 0-based to mirror ds,
-- converted at the string library's 1-based edges.

local function dv(c)
  if c == "0" then return 0
  elseif c == "1" then return 1
  elseif c == "2" then return 2
  elseif c == "3" then return 3
  elseif c == "4" then return 4
  elseif c == "5" then return 5
  elseif c == "6" then return 6
  elseif c == "7" then return 7
  elseif c == "8" then return 8
  else return 9 end
end

local function fold3(w)
  return dv(string.sub(w, 1, 1)) * 100 + dv(string.sub(w, 2, 2)) * 10 + dv(string.sub(w, 3, 3))
end

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
    sum = sum + fold3(string.sub(s, start + 1, j))
    start = j + 1
  end
end

print(string.format("result: %d %d", sum, 100000))
