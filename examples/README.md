# Examples

Runnable programs, one surface each, all gated end to end by `crates/ds-cli/tests/examples.rs`; run any of them with `ds run examples/<name>.ds`.

## Language core

- `hello.ds` — the minimal program: a greeting and an interpolation hole.
- `fib.ds` — naive recursion beside its tail-recursive variant, the 10^6-deep tail run completing on the constant stack.
- `lists.ds` — list literals, the `list[t]()` empty constructor, the checked read through `.ok`/`.err`, and the ambient generic `len`.
- `strings.ds` — the printable primitives converting through the interpolation boundary.
- `tuples.ds` — anonymous frame-span tuples with destructuring in bindings and match arms.
- `patterns.ds` — the match surfaces: struct, enum-case, nested struct, guard, and tuple patterns.
- `mutation.ds` — rebindable `var` locals, UFCS method calls, the counted loop, and the fallible `parse_int` extraction through `.err`.
- `containers.ds` — the container closes over CALLG dispatch: cons, tail, collect, the checked first, and max over both numeric instances.
- `records_lists.ds` — a record-element list over the native result span: the push/get members, `.ok`/`.err` extraction, and field projection out of the element.
- `script_entry.ds` — top-level statements folding into a synthesized `main` beside pure declarations, an early return ending the run.
- `schema.ds` — the comptime witness: functions running at compile time over declared fields and enum cases, their answers embedded as literals.

## Callables and generics

- `callables.ds` — named fns and capturing closures through fn-typed parameters, fn-typed returns as factories, trailing blocks, and fn collections over the scalar callable words.
- `combinators.ds` — map, filter, and reduce staging their lambda as one Fn word and reentering the dispatcher per element; the @std list pair (zip, enumerate) as bodied generic library fns.
- `slices.ds` — the @std slice pair (take, drop) clamping at the list's bounds through the bare and UFCS spellings, and flatten composing the flat_map spelling over map's hosted lambda (the fn-typed parameter's list return stays unsolved at B2), the fluent spelling chaining the pair through a dot-call's receiver.
- `generic_dispatch.ds` — transitive evidence: `impl[t: shows] tells for list[t]` lowers once and the concrete site stages the serving body's whole evidence area.
- `trait_defaults.ds` — defaulted parameters across every call spelling, the trait the member default's single authority and the impl block its inherent member's.

## JSON derive families

- `json.ds` — a round trip over the json face (the @std json member's source, composed back for every script session): parse_json, the from_json/to_json derives over a struct and an enum, absent optionals, and the parse_err rows.
- `json_width.ds` — the width-scalar fields through both derive families: byte and u32 as json numbers, char as a one-character json string, bytes as a json array of numbers, with range-edge and failure rows.

## Bytes

- `bytes.ds` — the bytes face: bytes_of, show, len, the checked byte_at, content `==`, the utf8 bridges, and the show derive over a bytes part.

## Concurrency

- `race.ds` — the @std `firstof`/`interleave` race combinators: a caller-built thunk list racing through the bounded channel, the arrival-order answers after the join-all boundary, and the `sum` fold over reduce's hosted callable.
- `nursery.ds` — spawn fan-out inside a nursery: the ordered join, the first failure cancelling the in-flight sibling, and the child error unioning out at the boundary.

## Packages

- `pub_import.ds` — the re-export chain's consumer: the middle package's pub import row joins the origin's callable, const, and struct onto its export surface (the aliased spelling included), the entry selecting them beside the middle's own member through one import row; a chain needs declared deps, so the run is a package entry over the staged tree (a plain script run declares no deps and binds nothing).

## Files and net

- `files.ds` — a write/read round trip through the file-IO natives, every fallible leg answering the `io_err` union (the write narrowed through `is`, the read handled through `.err`, the size extracted through `?`).
- `net.ds` — a TCP client flow over the linear net_handle: connect, send/recv, close — skipped with instructions and exit 0 when no port is staged with `--args`.
