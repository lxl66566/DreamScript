# Kernel: the lambda combinators — map/filter/reduce chains over
# list[int] and list[string] with non-capturing lambdas. Mirrors
# combinators.ds exactly: a 10000-element int list built by append, 250
# rounds of map (x*2), filter (x % 3 == 0), reduce (sum), then a
# 1000-element string list and 40 rounds of map (suffix concat), filter
# (len < 5), reduce (concat join), checksummed by length.

from functools import reduce

xs = []
for i in range(10000):
    xs.append((i * 37) % 900 + 100)

sum_ = 0
for _r in range(250):
    doubled = list(map(lambda x: x * 2, xs))
    kept = list(filter(lambda x: x % 3 == 0, doubled))
    sum_ += reduce(lambda a, b: a + b, kept, 0)

ys = []
for j in range(1000):
    ys.append(f"w{j % 40}")

lensum = 0
for _q in range(40):
    banged = list(map(lambda w: w + "!", ys))
    short = list(filter(lambda w: len(w) < 5, banged))
    joined = reduce(lambda a, b: a + b, short, "")
    lensum += len(joined)

print(f"result: {sum_} {len(xs)} {lensum} {len(ys)}")
