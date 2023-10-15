//! A module which decides which synchronization primitives to use throughout the rest of the crate
//! depending on features and configuration options

// std, non-test
#[cfg(all(feature = "std", not(loom), not(feature = "shuttle")))]
pub(crate) use std::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};

#[cfg(all(feature = "std", feature = "alloc", not(loom), not(feature = "shuttle")))]
pub(crate) use std::sync::Arc;

// no_std, non-test
#[cfg(all(not(loom), not(feature = "shuttle"), not(feature = "std")))]
pub(crate) use core::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};

#[cfg(all(feature = "alloc", not(loom), not(feature = "shuttle"), not(feature = "std")))]
pub(crate) use core::sync::Arc;

// loom
#[cfg(loom)]
pub(crate) use loom::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};

#[cfg(all(feature = "alloc", loom))]
pub(crate) use loom::sync::Arc;

// shuttle
#[cfg(feature = "shuttle")]
pub(crate) use shuttle::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};

#[cfg(feature = "shuttle")]
pub(crate) use shuttle::sync::{Arc};
