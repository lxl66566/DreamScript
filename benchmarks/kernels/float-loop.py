# Kernel: f64 loop — float mul/add/compare per iteration with a conditional
# rescale. Mirrors float-loop.ds exactly; the printed value is an odd fourth
# (x.25 or x.75), exact in binary and short in decimal, so every
# interpreter prints the same digits.

x = 0.25
half = 0.0
for _ in range(10000000):
    x = x * 1.5 + 0.25
    if x > 1000000.0:
        x = x * 0.25
        half = half + 0.5

print(f"result: {half + 0.25}")
