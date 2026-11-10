# Kernel: the closure surface — capturing closures built and called in
# hot loops. Mirrors closures.ds exactly: a named int factory (one
# closure per iteration), a const closure over an immutable local with
# the accumulator threaded through every call, a value-position factory
# ((a) => (b) => ... capturing through two levels), and a string
# factory (a reference-counted capture on the ds side). Captures stay
# immutable so by-reference capture here matches the ds by-value copy.

def make_adder(k):
    return lambda y: y + k

def make_suffix(s):
    return lambda w: w + s

sum1 = 0
for i in range(800000):
    add = make_adder(i % 97)
    sum1 += add(i % 1000)

k = 3
step = lambda acc, x: acc + x * k
acc = 0
for j in range(800000):
    acc = step(acc, j % 13)

mk = lambda a: (lambda b: a * b + a)
sum2 = 0
for q in range(800000):
    g = mk(q % 53)
    sum2 += g(q % 71)

lensum = 0
for w in range(400000):
    bang = make_suffix(f"-{w % 89}")
    tagged = bang(f"t{w % 17}")
    lensum += len(tagged)

print(f"result: {sum1} {acc} {sum2} {lensum}")
