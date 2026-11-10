# Kernel: hot mutation loop — integer mul/add/mod per iteration plus a parity
# branch. Mirrors loops-arith.ds exactly (Lehmer generator, every value below
# 2^47 so Python bignums and f64-only JS agree).

x = 1
acc = 0
for _ in range(10000000):
    x = (x * 48271) % 2147483647
    if x % 2 == 0:
        acc += x % 97
    else:
        acc += x % 89

print(f"result: {acc}")
