use crate::err::{Error, FatalError};
use crate::kcas::{HelpError, State};
use crate::sync::{AtomicPtr, AtomicUsize, Ordering};
use crate::types::{ThreadAndSequence, ThreadIndex, WordNum};
use core::fmt::Debug;
use displaydoc::Display;

use tracing::{instrument, trace};

/// Ensure that none of the target addresses contain the [ThreadAndSequence] marker for this
/// operation. Then, call `next_function`.
#[cfg_attr(feature = "tracing", instrument(skip(next_function)))]
pub(super) fn revert_and_transition<const NUM_THREADS: usize, const NUM_WORDS: usize, F>(
    shared_state: &State<NUM_THREADS, NUM_WORDS>,
    thread_index: ThreadIndex,
    thread_and_sequence: ThreadAndSequence,
    starting_word_num: WordNum,
    next_function: F,
) -> Result<(), Error>
where
    F: FnOnce() -> Result<(), Error>,
{
    match revert(
        shared_state,
        thread_index,
        thread_and_sequence,
        starting_word_num,
    ) {
        Ok(_) => next_function(),
        Err(revert_error) => match revert_error {
            RevertError::TargetAddressWasNotValidPointer {
                word_num,
                target_address,
            } => Err(Error::Fatal(FatalError::TargetAddressWasNotValidPointer {
                word_num,
                target_address,
            })),
        },
    }
}

/// Help another thread ensure that none of the target addresses contain the [ThreadAndSequence]
/// marker for this operation. Then, call `next_function`.
#[cfg_attr(feature = "tracing", instrument(skip(next_function)))]
pub(super) fn help_revert_and_transition<const NUM_THREADS: usize, const NUM_WORDS: usize, F>(
    shared_state: &State<NUM_THREADS, NUM_WORDS>,
    thread_index: ThreadIndex,
    thread_and_sequence: ThreadAndSequence,
    starting_word_num: WordNum,
    next_function: F,
) -> Result<(), HelpError>
where
    F: FnOnce() -> Result<(), HelpError>,
{
    match revert(
        shared_state,
        thread_index,
        thread_and_sequence,
        starting_word_num,
    ) {
        Ok(_) => next_function(),
        Err(revert_error) => match revert_error {
            RevertError::TargetAddressWasNotValidPointer {
                word_num,
                target_address,
            } => Err(HelpError::Fatal(
                FatalError::TargetAddressWasNotValidPointer {
                    word_num,
                    target_address,
                },
            )),
        },
    }
}

#[derive(Debug, Display)]
pub(super) enum RevertError {
    /// The target address {target_address} at word number {word_num} was not a valid pointer
    TargetAddressWasNotValidPointer {
        word_num: WordNum,
        target_address: usize,
    },
}

/// Ensure that none of the target addresses contain the [ThreadAndSequence] marker for this
/// operation.
#[instrument]
pub(super) fn revert<const NUM_THREADS: usize, const NUM_WORDS: usize>(
    shared_state: &State<NUM_THREADS, NUM_WORDS>,
    thread_index: ThreadIndex,
    thread_and_sequence: ThreadAndSequence,
    starting_word_num: WordNum,
) -> Result<(), RevertError> {
    for word_num in (0..=starting_word_num).rev() {
        trace!("thread_index {thread_index}: loading target address for word num {word_num}");
        let target_address_ptr: &AtomicPtr<AtomicUsize> =
            &shared_state.target_addresses[thread_index][word_num];
        let target_address_ptr: *mut AtomicUsize = target_address_ptr.load(Ordering::Acquire);

        let target_address: &AtomicUsize = unsafe { target_address_ptr.as_ref() }.ok_or(
            RevertError::TargetAddressWasNotValidPointer {
                word_num,
                target_address: target_address_ptr as usize,
            },
        )?;

        trace!("thread_index {thread_index}: loading expected element for word num {word_num}");
        let expected_element: usize =
            shared_state.expected_values[thread_index][word_num].load(Ordering::Acquire);

        trace!("thread_index {thread_index}: CASing expected element {expected_element} back for word num {word_num}");

        // even if this target address did not contain `thread_and_sequence`, keep trying to revert
        // the remaining addresses
        let result: Result<usize, usize> = target_address.compare_exchange(
            thread_and_sequence,
            expected_element,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
        match result {
            Ok(_) => trace!("thread_index {thread_index}: CAS succeeded for word num {word_num}"),
            Err(actual_value) => trace!(
                "thread_index {thread_index}: CAS failed for word num {word_num} with actual value {actual_value}"
            ),
        }
    }
    Ok(())
}
