-- Kernel: f64 loop — float mul/add/compare per iteration with a conditional
-- rescale. Mirrors float-loop.ds exactly; the printed value is an odd fourth
-- (x.25 or x.75), exact in binary and short in decimal, so every
-- interpreter prints the same digits (tostring's %.14g included).

local x = 0.25
local half = 0.0
for _ = 1, 10000000 do
  x = x * 1.5 + 0.25
  if x > 1000000.0 then
    x = x * 0.25
    half = half + 0.5
  end
end

print("result: " .. tostring(half + 0.25))
