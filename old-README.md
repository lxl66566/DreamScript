# DreamScript

## Preface

It's my inspiration of a new language, but not implemented yet. I hope one day I can make it up.

It's partially inspired by TypeScript. I really love TS + bun, because they has very easy to use grammar; but at last I still want a language made (think) by myself.

## Introduction

DreamScript is mainly aims at writing scripts, but it can do even more.

## Typing

DreamScript considers typing very important.

- Every type is a set; A variable could be a type or not to be a type, no third possibility.
- You can specify a new type using set instructions, such as:
  ```
  type A = B | C;           // B or C
  type A = B & C;           // B and C
  type A = B - C;           // A is B but not C
  type A = (B & C) - D;     // support arbitrary set instructions
  ```
- There is a special type: `any`. Every variable **is** any.
  - Using raw pointer or something to support `any` at the bottom layer.
- Every type should be determined on "compile time". If the compiler/runtime cannot infer the type of a variable, it will be treat as `any`.

## If/For

Just like Rust.

## Function

Each function is an object that could be assign, parse and modify.

To define a function is like to define a variable:

```
const f = (a: int) -> int {
  ? a;
}

// You can put bare expression at the end as return value. The above is same as:

const f = (a: int) -> int {
  a
}
```

Using `?` to return a value. Note that we don't use `function` or `return` as keyword.

### Generator

A Generator is same as an _Iterator_ like other language.

```
const f = (a: int) -*> int {
  for i in 0..a {
    yield a;
  }
}
```

We use `-*>` to make a Generator, `?` (or leave blank) to return (finish) the Generator.

The Generator will support full instructions like [Rust Iterator](https://doc.rust-lang.org/std/iter/trait.Iterator.html).

## Error Handling

Each error is a type but can be throw in the other demension. If an error occurs, it will first try to throw it.

```
const b = 1 / 0;    // Will panic! No one deal with it!

const f = () -> int {
  ? 1 / 0;          // Will panic! No one deal with it!
}
const b: int | ArithmeticError = f();     // Will panic! The error should be deal in the instant block,
                                          // i.e. in function `f`.

const f = () -> int | ArithmeticError {
  const b = 1 / 0;  // Because the current function (block) return type has `ArithmeticError`,
                    // the error will be `collect` to a variable that has type `ArithmeticError`, then return.
  ? b + 2;
}

const b = { 1 / 0 };  // Will panic! The b is inferred as `double` type,
                      // which is not able to deal with it.
const b: double | ArithmeticError = { 1 / 0 };    // That's correct.
```

You can re-throw an Error which has been catched:

```
const a: double | ArithmeticError = 1 / 0;
throw a;
```

### dealing

You can deal an error in a block or in a variable. The basic functions are `ok((T) -> R)` and `err((T) -> R)`.

```
const result = {1 / 0}.err((e) -> println(e));    // If the block is ok, returns it; otherwise returns the
                                                  // return value of the function parsed to `err()`.
                                                  // So the result type is inferred as `double | null`.

const result: null | ArithmeticError = {1 / 2}.ok((v) -> println(v));
// The result will be `null`.
```

Then you can deal with the `null` using other ways.

If you want to deal with an Error in a function but not throw it, just wrap it in a block:

```
const f = (to_be_div: int) -> int | null | ArithmeticError {
  {
    const b = 1 / to_be_div;
    b + 2
  }.err((e) -> println(`Error: {e}`))
}
const result = f(0);    // ArithmeticError has been handled. The `println` returns null, so result == null.
```
