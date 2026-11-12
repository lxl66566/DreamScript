# Kernel: the fiber path — spawn/join nursery cycles. Mirrors nursery.ds
# exactly with asyncio: each cycle awaits a gather of the two child
# coroutines (asyncio wraps each in a task, the spawn analogue) and the
# gather return is the join; every 100th cycle a child of a child nests
# a second gather (the interlock's join-across-levels shape). The final
# walk re-reads the shared list to checksum the children's answers.

import asyncio

N = 200000
LABEL = "fiber-probe"


async def child_a(out, i):
    out.append((i % 97) + (i % 13))


async def child_b(out, i, label):
    out.append(len(label) + i % 11)


async def deep(i, label):
    sub = []

    async def grandchild():
        sub.append(len(label) + i % 7)

    async def child():
        await asyncio.gather(grandchild())

    await asyncio.gather(child())
    return sub[0] if sub else 0


async def main():
    out = []
    deep_sum = 0
    for i in range(N):
        await asyncio.gather(child_a(out, i), child_b(out, i, LABEL))
        if i % 100 == 99:
            deep_sum += await deep(i, LABEL)
    s = sum(out)
    print(f"result: {s} {deep_sum} {len(out)}")


asyncio.run(main())
