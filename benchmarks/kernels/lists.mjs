// Kernel: the list surface — array split, indexed gets, and repeated
// whole-array joins. Mirrors lists.ds exactly: 20000 three-digit pieces
// built through interpolation (the empty-string seed), one split,
// 120 checksum rounds of indexed reads with the same char digit fold as
// strings.mjs (the ds parse native's success-typed union answers no
// runtime extraction yet), then 32 array concatenations into a fresh
// array each time and a full sweep of the 660000-element result.

function dv(c) {
  if (c === "0") return 0;
  else if (c === "1") return 1;
  else if (c === "2") return 2;
  else if (c === "3") return 3;
  else if (c === "4") return 4;
  else if (c === "5") return 5;
  else if (c === "6") return 6;
  else if (c === "7") return 7;
  else if (c === "8") return 8;
  else return 9;
}

function fold3(w) {
  return dv(w.charAt(0)) * 100 + dv(w.charAt(1)) * 10 + dv(w.charAt(2));
}

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
    sum += fold3(parts[i]);
  }
}

let acc = parts;
for (let c = 0; c < 32; c++) {
  acc = acc.concat(parts);
}

let total = 0;
for (let j = 0; j < 20000 * 33; j++) {
  total += fold3(acc[j]);
}

console.log(`result: ${sum} ${total} ${acc.length}`);
