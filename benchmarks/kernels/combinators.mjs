// Kernel: the lambda combinators — map/filter/reduce chains over int
// and string arrays. Mirrors combinators.ds exactly: a 10000-element
// int list built by push, 250 rounds of map (x*2), filter (x % 3 === 0),
// reduce (sum), then a 1000-element string list and 40 rounds of map
// (suffix concat), filter (length < 5), reduce (concat join),
// checksummed by length.

let xs = [];
for (let i = 0; i < 10000; i++) {
  xs.push((i * 37) % 900 + 100);
}

let sum = 0;
for (let r = 0; r < 250; r++) {
  const doubled = xs.map((x) => x * 2);
  const kept = doubled.filter((x) => x % 3 === 0);
  sum += kept.reduce((a, b) => a + b, 0);
}

let ys = [];
for (let j = 0; j < 1000; j++) {
  ys.push(`w${j % 40}`);
}

let lensum = 0;
for (let q = 0; q < 40; q++) {
  const banged = ys.map((w) => w + "!");
  const short = banged.filter((w) => w.length < 5);
  const joined = short.reduce((a, b) => a + b, "");
  lensum += joined.length;
}

console.log(`result: ${sum} ${xs.length} ${lensum} ${ys.length}`);
