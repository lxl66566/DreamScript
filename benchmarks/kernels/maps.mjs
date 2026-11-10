// Kernel: the interim maps surface — set/get/size builder-call
// sequences over string keys by content. Mirrors maps.ds exactly: one
// Map of 5000 distinct keys filled, 16 read rounds over all keys
// (undefined answers map to -1 like the ds checked read's none side),
// 2000 missing-key probes, a key-for-key overwrite pass observing the
// size per set, and one final read round pinning the overwritten
// values.

const m = new Map();
for (let i = 0; i < 5000; i++) {
  m.set(`key-${i % 7}-${i}`, (i * 37) % 900 + 100);
}

let sum = 0;
for (let r = 0; r < 16; r++) {
  for (let j = 0; j < 5000; j++) {
    const v = m.get(`key-${j % 7}-${j}`);
    sum += v === undefined ? -1 : v;
  }
}

let miss = 0;
for (let q = 0; q < 2000; q++) {
  const v = m.get(`missing-${q}`);
  miss += v === undefined ? -1 : v;
}

let n2 = 0;
for (let k = 0; k < 5000; k++) {
  m.set(`key-${k % 7}-${k}`, k % 100);
  n2 += m.size;
}

let sum2 = 0;
for (let w = 0; w < 5000; w++) {
  const v = m.get(`key-${w % 7}-${w}`);
  sum2 += v === undefined ? -1 : v;
}

console.log(`result: ${sum} ${miss} ${m.size} ${n2} ${sum2}`);
