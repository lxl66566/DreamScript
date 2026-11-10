# Kernel: the list surface — list split, indexed gets, and repeated
# whole-list joins. Mirrors lists.ds exactly: 20000 three-digit pieces
# built through interpolation (the empty-string seed), one split,
# 120 checksum rounds of indexed reads with the same char digit fold as
# strings.py (the ds parse native's success-typed union answers no
# runtime extraction yet), then 32 list concatenations into a fresh
# list each time and a full sweep of the 660000-element result.

def dv(c):
    if c == "0":
        return 0
    elif c == "1":
        return 1
    elif c == "2":
        return 2
    elif c == "3":
        return 3
    elif c == "4":
        return 4
    elif c == "5":
        return 5
    elif c == "6":
        return 6
    elif c == "7":
        return 7
    elif c == "8":
        return 8
    else:
        return 9

def fold3(w):
    return dv(w[0]) * 100 + dv(w[1]) * 10 + dv(w[2])

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
        sum_ += fold3(parts[i])

acc = parts
for _c in range(32):
    acc = acc + parts

total = 0
for j in range(20000 * 33):
    total += fold3(acc[j])

print(f"result: {sum_} {total} {len(acc)}")
