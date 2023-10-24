//! A module which decides which synchronization primitives to use throughout the rest of the crate
//! depending on features and configuration options

cfg_if::cfg_if! {
    if #[cfg(loom)] {
        pub(crate) use loom::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};
        pub(crate) use loom::sync::Arc;
    } else if #[cfg(feature = "shuttle")] {
        pub(crate) use shuttle::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};
        pub(crate) use shuttle::sync::Arc;
    } else if #[cfg(feature = "std")] {
        pub(crate) use std::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};
        pub(crate) use std::sync::Arc;
    } else if #[cfg(feature = "alloc")] {
        pub(crate) use core::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};
        pub(crate) use alloc::sync::Arc;
    } else {
        pub(crate) use core::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};
    }
}
