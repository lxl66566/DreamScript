-- Kernel: hot mutation loop — integer mul/add/mod per iteration plus a parity
-- branch. Mirrors loops-arith.ds exactly (Lehmer generator, every value below
-- 2^47 so i64 Lua and f64-only JS agree).

local x = 1
local acc = 0
for _ = 1, 10000000 do
  x = (x * 48271) % 2147483647
  if x % 2 == 0 then
    acc = acc + x % 97
  else
    acc = acc + x % 89
  end
end

print(string.format("result: %d", acc))
