# Kernel: build strings then reprocess them — f-string interpolation with two
# holes, concat, strip, char scanning, slicing cuts, a char digit fold. Mirrors
# strings.ds exactly: pieces are three digits plus ';', the fold's positional
# weights (100/10/1) reconstruct the piece value int parsing would answer
# (the digit ladder replaces the parse native on the ds side), accumulators
# seed with the empty string literal.

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
        sum_ += fold3(s[start:j])
        start = j + 1

print(f"result: {sum_} {100000}")
