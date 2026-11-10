# Kernel: the interim maps surface — set/get/len builder-call sequences
# over string keys by content. Mirrors maps.ds exactly: one dict of
# 5000 distinct keys filled, 16 read rounds over all keys (misses map
# to -1 like the ds checked read's none side), 2000 missing-key probes,
# a key-for-key overwrite pass observing the length per set, and one
# final read round pinning the overwritten values.

m = {}
for i in range(5000):
    m[f"key-{i % 7}-{i}"] = (i * 37) % 900 + 100

sum_ = 0
for _r in range(16):
    for j in range(5000):
        sum_ += m.get(f"key-{j % 7}-{j}", -1)

miss = 0
for q in range(2000):
    miss += m.get(f"missing-{q}", -1)

n2 = 0
for k in range(5000):
    m[f"key-{k % 7}-{k}"] = k % 100
    n2 += len(m)

sum2 = 0
for w in range(5000):
    sum2 += m.get(f"key-{w % 7}-{w}", -1)

print(f"result: {sum_} {miss} {len(m)} {n2} {sum2}")
