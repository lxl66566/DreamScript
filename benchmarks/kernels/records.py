# Kernel: the record span channel. Mirrors records.ds exactly: a list
# of N four-field dicts (two ints, a float, a string) built by append,
# then R read passes projecting the fields per element, a missing-index
# probe over the same checked read (the fallback dict answers the none
# side), and the first element pinned. Scores cycle 0.25/0.75/1.25 over
# an odd pass count so the printed float total is an odd fourth.

n = 20001
xs = []
for i in range(n):
    m = i % 3
    score = 0.25
    if m == 1:
        score = 0.75
    if m == 2:
        score = 1.25
    tv = (i % 40) + 10
    xs.append({"id": (i * 37) % 900 + 100, "w": i % 7, "score": score, "name": f"t{tv}"})

blank = {"id": -1, "w": 0, "score": 0.25, "name": "t00"}
sum_id = 0
sum_w = 0
lensum = 0
total = 0.0
for _r in range(45):
    for j in range(n):
        p = xs[j]
        sum_id += p["id"]
        sum_w += p["w"]
        total += p["score"]
        lensum += len(p["name"])

miss = 0
for q in range(2000):
    p = xs[n + q] if n + q < len(xs) else blank
    miss += p["id"]

hid = xs[0]["id"] if xs else -1

print(f"result: {sum_id} {sum_w} {lensum} {miss} {hid} {total}")
