//! Instruction-count pin host.
//!
//! The crate carries no library code: `benches/` holds the gungraun kernels
//! over ds-vm's interpreter paths and `tests/pin_bake.rs` asserts their
//! committed Callgrind counts on every optimized-profile gate run. Both live
//! outside ds-vm so the harness's dependency graph never reaches ds-vm's test
//! binaries. `examples/opcode_census.rs` is the superinstruction evidence tool: it lowers
//! the benchmark kernels through the compiler chain and prints the opcode
//! frequencies and adjacent-pair coverage a fusion has to be chosen from.
