#[cfg(all(feature = "std", not(loom)))]
pub(crate) use std::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};

#[cfg(loom)]
pub(crate) use loom::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};

#[cfg(not(any(loom, feature = "std")))]
pub(crate) use core::sync::atomic::{AtomicUsize, AtomicPtr, Ordering};
