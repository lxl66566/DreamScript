// Kernel: build strings then reprocess them — template interpolation with
// two holes, concat, charAt scanning, substring cuts, a char digit fold.
// Mirrors strings.ds exactly: pieces are three digits plus ';', the fold's
// positional weights (100/10/1) reconstruct the piece value Number parsing
// would answer (the digit ladder replaces the parse native on the ds side),
// accumulators seed with the empty string literal.

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
    sum += fold3(s.substring(start, j));
    start = j + 1;
  }
}

console.log(`result: ${sum} ${100000}`);
