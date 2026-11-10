# Kernel: the container closes — cons/head/tail/collect/first/max over
# ints. Mirrors containers.ds exactly: a prepend ladder builds a
# 15000-element list (each prepend copies the whole predecessor), a
# head/tail drain walks it down one element per iteration keeping a
# running max (the slice copies the remainder each step), 150 fresh
# copies re-read the full list, and the first element pins the head.
# The empty answers map the ds checked read's none side to -1.

xs = []
for i in range(15000):
    xs = [(i * 37) % 900 + 100] + xs

m = 0
rest = xs
c = 0
while c < 15000:
    v = rest[0] if rest else -1
    m = max(m, v)
    rest = rest[1:]
    c += 1

total = 0
for _r in range(150):
    ys = list(xs)
    total += len(ys)

first_v = xs[0] if xs else -1

print(f"result: {len(xs)} {m} {total} {first_v}")
