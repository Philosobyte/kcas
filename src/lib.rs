//! # KCAS
//!
//! A lock-free multi-word compare-and-swap library. It is `no_std`-compatible and optionally does
//! not allocate on the heap. Because it only requires single-width atomic compare-and-swap, it is
//! lock-free on most platforms, including RISC-V, AArch64, and x86-64.
//!
//! # Usage
//! ## Example
//! Here is an example using [RefStateWrapper]:
//! ```edition2021
//! use kcas::{KCasWord, State, RefStateWrapper};
//! use kcas::Error;
//! use std::sync::atomic::{AtomicUsize, Ordering};
//! use std::thread::{ScopedJoinHandle};
//!
//! // Allocate memory on the stack for 2 threads, each operating on 3 words.
//! let state: State<2, 3> = State::new();
//!
//! // each thread gets its own wrapper
//! let mut first_wrapper: RefStateWrapper<2, 3> = RefStateWrapper::construct(&state).unwrap();
//! let mut second_wrapper: RefStateWrapper<2, 3> = RefStateWrapper::construct(&state).unwrap();
//!
//! let first_target: AtomicUsize = AtomicUsize::new(1);
//! let second_target: AtomicUsize = AtomicUsize::new(2);
//! let third_target: AtomicUsize = AtomicUsize::new(3);
//!
//! // run k-CAS operations in two threads
//! std::thread::scope(|scope| {
//!     let first_handle: ScopedJoinHandle<Result<(), Error>> = scope.spawn(|| {
//!         first_wrapper.kcas([
//!             KCasWord::new(&first_target, 1, 4),
//!             KCasWord::new(&second_target, 2, 5),
//!             KCasWord::new(&third_target, 3, 6),
//!         ])
//!     });
//!     let second_handle: ScopedJoinHandle<Result<(), Error>> = scope.spawn(|| {
//!         second_wrapper.kcas([
//!             KCasWord::new(&first_target, 1, 7),
//!             KCasWord::new(&second_target, 2, 8),
//!             KCasWord::new(&third_target, 3, 9),
//!         ])
//!     });
//! });
//!
//! // one of the two operations should succeed
//! assert!(
//!     (first_target.load(Ordering::Acquire) == 4
//!         && second_target.load(Ordering::Acquire) == 5
//!         && third_target.load(Ordering::Acquire) == 6
//!     ) || (
//!         first_target.load(Ordering::Acquire) == 7
//!         && second_target.load(Ordering::Acquire) == 8
//!         && third_target.load(Ordering::Acquire) == 9
//!     )
//! );
//! ```
//!
//! ## Details
//! Begin by instantiating a [State], which is a struct shared between threads so threads can
//! assist each other. [State] reserves all the memory it needs to share during initialization.
//! [State] requires two generic const arguments:
//! - `NUM_THREADS`, the maximum number of threads which can operate on that State at any given
//! time, and
//! - `NUM_WORDS`, the number of words to operate upon during each k-CAS operation.
//!
//! See the [Limitations] section for limits on these parameters.
//!
//! Next, wrap [State] in a "wrapper" implementation. Each wrapper reserves a thread id from the
//! underlying [State] and thus is designed to execute k-CAS operations for exactly one thread at a
//! time. There are three wrapper implementations:
//! - [RefStateWrapper]'s constructor takes a `&State` where [State] typically lives on the stack.
//! [std::thread::scope] allows borrowing [RefStateWrapper] across threads.
//! - [ArcStateWrapper]'s constructor takes `Arc<State>` where [State] lives on the
//! heap and the `Arc` is cloned and moved across threads. This can only be used in a
//! `std` environment or a `no_std` environment which uses [alloc].
//! - [UnsafeStateWrapper]'s constructor takes a `NonNull<State>` for greater control
//! over where [State] lives and how it is used across threads.
//!
//! Next, call the wrapper's `kcas` method, which performs the k-CAS operation defined by its
//! argument, a [KCasWord] array of size `NUM_WORDS`. Each individual CAS operation occurs in order.
//!
//! The wrapper can be dropped once all k-CAS operations are finished for a particular thread. The
//! drop call will return the wrapper's thread id back into the pool of available thread ids.
//!
//! # Limitations
//! ## Target addresses must be used across threads in the same order
//! When two target addresses are passed into k-CAS operations in two threads in the opposite order,
//! a deadlock can occur. As a result, any addresses used in two or more threads must be passed in
//! the same order.
//!
//! To illustrate, let's say we would like a thread to perform a 3-CAS operation across three target
//! addresses `(1, 2, 3)`. If another thread is also performing operations, this second thread can
//! operate on any of the first thread's addresses individually in any position - for example, the
//! second thread could operate across `(4, 1, 5)`, `(4, 5, 2)`, or `(3, 4, 5)`.  But, if the second
//! thread would like to operate on two of the first thread's addresses, those addresses must be
//! specified in the same order. In this case, `(1, 2, 4)`, `(4, 1, 3)`, `(1, 5, 3)` are examples of
//! valid orderings. However, `(2, 1, 4)` is not a valid ordering because `1` was specified before
//! `2` in the first thread, but `1` was specified after `2` in the second thread.
//!
//! ## The number of bits used by values bounds the size of `NUM_THREADS`
//! While there is no limit to the size of `NUM_WORDS` besides memory consumption, `NUM_THREADS` is
//! limited by the number of value bits you would like to use in your application for real content.
//! As an intermediate step, KCAS swaps tagged markers into the provided target addresses where the
//! most significant bits are a thread id and the least significant bits are a sequence number.
//! KCAS differentiates between tagged markers and real values by looking at the most significant
//! bits: if they are all 0s or all 1s, then they are real values; otherwise, they are treated as
//! tagged markers.
//!
//! This limitation makes the KCAS library unsuitable for performing k-CAS operations on
//! random values which must take up all bits of a usize. This also limits the number of threads
//! available for performing k-CAS operations on pointers. For example, on 64-bit systems,
//! - if you would like to perform k-CAS on 48-bit x86-64 pointers and the top 16 bits of the value
//! will always be all 0s or all 1s, `NUM_THREADS` can be as high as `2^12` or `65536`.
//! - for 52-bit ARMv8.2 LVA pointers, you may set `NUM_THREADS` as high as `2^12` or `4096`.
//! - for 57-bit 5-level paging pointers, you may set `NUM_THREADS` as high as `2^7` or `128`.
//!
//! To programmatically determine how many value bits are reserved by the KCAS library, you may call
//! [get_bit_length_of_num_threads].
//!
//! We plan to remove this limitation in the future by introducing an alternative implementation
//! where tagged markers do not scale with the number of threads. This design will rely on a
//! different mechanism for helping threads which should make the algorithm wait-free.
//!
#![warn(missing_debug_implementations, missing_docs)]

mod err;
mod kcas;
mod sync;
mod types;
mod wrapper;

pub use err::{Error, FatalError};
pub use kcas::{KCasWord, State};
pub use types::{get_bit_length_of_num_threads, is_value_a_kcas_marker};
pub use wrapper::{
    ArcStateWrapper, ArcStateWrapperError, RefStateWrapper, RefStateWrapperError,
    UnsafeStateWrapper, UnsafeStateWrapperError,
};
