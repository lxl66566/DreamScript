// Kernel: f64 loop — float mul/add/compare per iteration with a conditional
// rescale. Mirrors float-loop.ds exactly; the printed value is an odd fourth
// (x.25 or x.75), exact in binary and short in decimal, so every
// interpreter prints the same digits.

let x = 0.25;
let half = 0.0;
for (let i = 0; i < 10000000; i++) {
  x = x * 1.5 + 0.25;
  if (x > 1000000.0) {
    x = x * 0.25;
    half = half + 0.5;
  }
}

console.log(`result: ${half + 0.25}`);
