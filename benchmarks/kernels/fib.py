# Kernel: naive recursive fib — call overhead and integer add per call.
# Mirrors fib.ds exactly (fib(34), i64/f64-exact range).


def fib(n):
    if n < 2:
        return n
    return fib(n - 1) + fib(n - 2)


print(f"result: {fib(34)}")
