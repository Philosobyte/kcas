# KCAS

[![build status]][actions]
[![docs]][`kcas` documentation]
[![rustc version 1.63+]][rust 1.63]
[![kcas latest version]][kcas crates.io]

A lock-free multi-word compare-and-swap library. It is `no_std`-compatible and optionally does
not allocate on the heap. Because it only requires single-width atomic compare-and-swap, it is
lock-free on most platforms, including RISC-V, AArch64, and x86-64.

## Getting started
To use `kcas` in your project, include the following in `Cargo.toml`.
```toml
[dependencies]
kcas = "0.1.0"
```
For a `no_std` project, additionally set `default-features = false`. If your `no_std` project 
allows allocation, you may set `features = ["alloc"]` for more options.

Please see [`kcas` documentation] for examples and details about how to use the library.

## Planned work
1. additional testing - `KCAS` passes billions of iterations of [Shuttle] tests, but because Shuttle assumes `SeqCst` 
consistency, we should also test using Loom. Additionally, while `KCAS` passes regular concurrency tests on x86-64, we 
would also like to run tests on AArch64 and RISC-V.
2. benchmarking - `KCAS`'s allocation freedom comes at the cost of requiring more CAS operations than state of the art 
multi-word CAS algorithms. We would like to benchmark `KCAS` against other multi-word CAS algorithms in various 
scenarios to illustrate the behavior in various scenarios, including both unconstrained and memory-constrained 
scenarios.
3. wait-free implementation - As described in the `Limitations` section of the documentation, `KCAS` is not currently
suitable for use in situations which require both a large number of bits allocated to the value and a large number of 
threads. An alternative helper mechanism is planned which will reduce the number of bits shaved off of the value to 1.
This mechanism may also make the algorithm wait-free.

## License
This library may be used under either the [MIT License] or [Apache License (Version 2.0)].
