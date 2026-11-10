// Kernel: hot mutation loop — integer mul/add/mod per iteration plus a parity
// branch. Mirrors loops-arith.ds exactly (Lehmer generator, every value below
// 2^47 so f64-only JS stays exact).

let x = 1;
let acc = 0;
for (let i = 0; i < 10000000; i++) {
  x = (x * 48271) % 2147483647;
  if (x % 2 === 0) {
    acc += x % 97;
  } else {
    acc += x % 89;
  }
}

console.log(`result: ${acc}`);
