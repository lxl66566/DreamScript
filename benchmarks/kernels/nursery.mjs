// Kernel: the fiber path — spawn/join nursery cycles. Mirrors nursery.ds
// exactly over promise microtasks: each child call is an async function
// (its body runs as a scheduled microtask, the spawn analogue) and each
// cycle awaits Promise.all over the two children (the join); every
// 100th cycle a child of a child nests a second Promise.all (the
// interlock's join-across-levels shape). The final walk re-reads the
// shared array to checksum the children's answers.

const N = 200000;
const LABEL = "fiber-probe";

async function childA(out, i) {
  out.push((i % 97) + (i % 13));
}

async function childB(out, i, label) {
  out.push(label.length + (i % 11));
}

async function deep(i, label) {
  const sub = [];

  const grandchild = async () => {
    sub.push(label.length + (i % 7));
  };

  const child = async () => {
    await Promise.all([grandchild()]);
  };

  await Promise.all([child()]);
  return sub.length > 0 ? sub[0] : 0;
}

async function main() {
  const out = [];
  let deepSum = 0;
  for (let i = 0; i < N; i++) {
    await Promise.all([childA(out, i), childB(out, i, LABEL)]);
    if (i % 100 === 99) {
      deepSum += await deep(i, LABEL);
    }
  }
  let s = 0;
  for (const v of out) {
    s += v;
  }
  console.log(`result: ${s} ${deepSum} ${out.length}`);
}

main();
