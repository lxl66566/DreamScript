# Kernel: build strings then reprocess them — f-string interpolation with two
# holes, concat, strip, char scanning, slicing cuts, int parsing. Mirrors
# strings.ds exactly: pieces are three digits plus ';', accumulators seed
# with the empty string literal.

s = ""
for gi in range(25):
    piece = ""
    for k in range(1000):
        v = (gi * 1000 + k) * 37 % 900 + 100
        piece = f"{piece}{v};"
    s = s + piece

sum_ = 0
start = 0
for j in range(100000):
    if s[j] == ";":
        sum_ += int(s[start:j])
        start = j + 1

print(f"result: {sum_} {100000}")
