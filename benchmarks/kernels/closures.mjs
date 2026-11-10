// Kernel: the closure surface — capturing closures built and called in
// hot loops. Mirrors closures.ds exactly: a named int factory (one
// closure per iteration), a const closure over an immutable local with
// the accumulator threaded through every call, a value-position factory
// ((a) => (b) => ... capturing through two levels), and a string
// factory (a reference-counted capture on the ds side). Captures stay
// immutable so by-reference capture here matches the ds by-value copy.

function makeAdder(k) {
  return (y) => y + k;
}

function makeSuffix(s) {
  return (w) => w + s;
}

let sum1 = 0;
for (let i = 0; i < 800000; i++) {
  const add = makeAdder(i % 97);
  sum1 += add(i % 1000);
}

const k = 3;
const step = (acc, x) => acc + x * k;
let acc = 0;
for (let j = 0; j < 800000; j++) {
  acc = step(acc, j % 13);
}

const mk = (a) => (b) => a * b + a;
let sum2 = 0;
for (let q = 0; q < 800000; q++) {
  const g = mk(q % 53);
  sum2 += g(q % 71);
}

let lensum = 0;
for (let w = 0; w < 400000; w++) {
  const bang = makeSuffix(`-${w % 89}`);
  const tagged = bang(`t${w % 17}`);
  lensum += tagged.length;
}

console.log(`result: ${sum1} ${acc} ${sum2} ${lensum}`);
