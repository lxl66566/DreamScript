-- Kernel: naive recursive fib — call overhead and integer add per call.
-- Mirrors fib.ds exactly (fib(34), i64/f64-exact range).

local function fib(n)
  if n < 2 then
    return n
  end
  return fib(n - 1) + fib(n - 2)
end

print(string.format("result: %d", fib(34)))
