// Kernel: the list surface — array split, indexed gets, and repeated
// whole-array joins. Mirrors lists.ds exactly: 20000 three-digit pieces
// built through interpolation (the empty-string seed), one split,
// 120 checksum rounds of indexed reads with Number parsing, then 32
// array concatenations into a fresh array each time and a full sweep of
// the 660000-element result.

let s = "";
for (let gi = 0; gi < 20; gi++) {
  let piece = "";
  for (let k = 0; k < 1000; k++) {
    const v = (gi * 1000 + k) * 37 % 900 + 100;
    piece = `${piece}${v};`;
  }
  s = s + piece;
}
s = s.substring(0, 20000 * 4 - 1);

const parts = s.split(";");

let sum = 0;
for (let p = 0; p < 120; p++) {
  for (let i = 0; i < 20000; i++) {
    sum += Number(parts[i]);
  }
}

let acc = parts;
for (let c = 0; c < 32; c++) {
  acc = acc.concat(parts);
}

let total = 0;
for (let j = 0; j < 20000 * 33; j++) {
  total += Number(acc[j]);
}

console.log(`result: ${sum} ${total} ${acc.length}`);
