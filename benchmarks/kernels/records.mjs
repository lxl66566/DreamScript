// Kernel: the record span channel. Mirrors records.ds exactly: a list
// of N four-field objects (two ints, a float, a string) built by push,
// then R read passes projecting the fields per element, a missing-index
// probe over the same checked read (the fallback object answers the
// none side), and the first element pinned. Scores cycle
// 0.25/0.75/1.25 over an odd pass count so the printed float total is
// an odd fourth.

const n = 20001;
const xs = [];
for (let i = 0; i < n; i++) {
  const m = i % 3;
  let score = 0.25;
  if (m === 1) {
    score = 0.75;
  }
  if (m === 2) {
    score = 1.25;
  }
  const tv = (i % 40) + 10;
  xs.push({ id: (i * 37) % 900 + 100, w: i % 7, score: score, name: `t${tv}` });
}

const blank = { id: -1, w: 0, score: 0.25, name: "t00" };
let sumId = 0;
let sumW = 0;
let lensum = 0;
let total = 0.0;
for (let r = 0; r < 45; r++) {
  for (let j = 0; j < n; j++) {
    const p = xs[j];
    sumId += p.id;
    sumW += p.w;
    total += p.score;
    lensum += p.name.length;
  }
}

let miss = 0;
for (let q = 0; q < 2000; q++) {
  const p = n + q < xs.length ? xs[n + q] : blank;
  miss += p.id;
}

const hid = xs.length > 0 ? xs[0].id : -1;

console.log(`result: ${sumId} ${sumW} ${lensum} ${miss} ${hid} ${total}`);
