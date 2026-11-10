-- Kernel: the closure surface — capturing closures built and called in
-- hot loops. Mirrors closures.ds exactly: a named int factory (one
-- closure per iteration), a const closure over an immutable local with
-- the accumulator threaded through every call, a value-position factory
-- ((a) => (b) => ... capturing through two levels), and a string
-- factory (a reference-counted capture on the ds side). Captures stay
-- immutable so by-reference upvalues here match the ds by-value copy.

local function make_adder(k)
  return function(y) return y + k end
end

local function make_suffix(s)
  return function(w) return w .. s end
end

local sum1 = 0
for i = 0, 799999 do
  local add = make_adder(i % 97)
  sum1 = sum1 + add(i % 1000)
end

local k = 3
local step = function(acc, x) return acc + x * k end
local acc = 0
for j = 0, 799999 do
  acc = step(acc, j % 13)
end

local mk = function(a) return function(b) return a * b + a end end
local sum2 = 0
for q = 0, 799999 do
  local g = mk(q % 53)
  sum2 = sum2 + g(q % 71)
end

local lensum = 0
for w = 0, 399999 do
  local bang = make_suffix(string.format("-%d", w % 89))
  local tagged = bang(string.format("t%d", w % 17))
  lensum = lensum + #tagged
end

print(string.format("result: %d %d %d %d", sum1, acc, sum2, lensum))
