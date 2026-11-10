# Kernel: the list surface — list split, indexed gets, and repeated
# whole-list joins. Mirrors lists.ds exactly: 20000 three-digit pieces
# built through interpolation (the empty-string seed), one split,
# 120 checksum rounds of indexed reads with int parsing, then 32 list
# concatenations into a fresh list each time and a full sweep of the
# 660000-element result.

s = ""
for gi in range(20):
    piece = ""
    for k in range(1000):
        v = (gi * 1000 + k) * 37 % 900 + 100
        piece = f"{piece}{v};"
    s = s + piece
s = s[:20000 * 4 - 1]

parts = s.split(";")

sum_ = 0
for _p in range(120):
    for i in range(20000):
        sum_ += int(parts[i])

acc = parts
for _c in range(32):
    acc = acc + parts

total = 0
for j in range(20000 * 33):
    total += int(acc[j])

print(f"result: {sum_} {total} {len(acc)}")
