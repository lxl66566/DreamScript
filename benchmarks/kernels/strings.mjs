// Kernel: build strings then reprocess them — template interpolation with
// two holes, concat, charAt scanning, substring cuts, Number parsing.
// Mirrors strings.ds exactly: pieces are three digits plus ';',
// accumulators seed with the empty string literal.

let s = "";
for (let gi = 0; gi < 25; gi++) {
  let piece = "";
  for (let k = 0; k < 1000; k++) {
    const v = (gi * 1000 + k) * 37 % 900 + 100;
    piece = `${piece}${v};`;
  }
  s = s + piece;
}

let sum = 0;
let start = 0;
for (let j = 0; j < 100000; j++) {
  if (s.charAt(j) === ";") {
    sum += Number(s.substring(start, j));
    start = j + 1;
  }
}

console.log(`result: ${sum} ${100000}`);
