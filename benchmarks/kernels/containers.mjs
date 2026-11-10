// Kernel: the container closes — cons/head/tail/collect/first/max over
// ints. Mirrors containers.ds exactly: a prepend ladder builds a
// 15000-element array (each prepend copies the whole predecessor), a
// head/tail drain walks it down one element per iteration keeping a
// running max (slice copies the remainder each step), 150 fresh copies
// re-read the full array, and the first element pins the head. The
// empty answers map the ds checked read's none side to -1.

let xs = [];
for (let i = 0; i < 15000; i++) {
  xs = [(i * 37) % 900 + 100].concat(xs);
}

let m = 0;
let rest = xs;
let c = 0;
while (c < 15000) {
  const v = rest.length > 0 ? rest[0] : -1;
  m = Math.max(m, v);
  rest = rest.slice(1);
  c += 1;
}

let total = 0;
for (let r = 0; r < 150; r++) {
  const ys = xs.slice();
  total += ys.length;
}

const firstV = xs.length > 0 ? xs[0] : -1;

console.log(`result: ${xs.length} ${m} ${total} ${firstV}`);
