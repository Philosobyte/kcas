use crate::aliases::{ThreadAndSequence, ThreadIndex, WordNum};
use crate::err::{FatalError, KCasError};
use crate::kcas::{HelpError, SharedState};
use crate::sync::{AtomicPtr, AtomicUsize, Ordering};

pub(super) fn revert_and_transition<const NUM_THREADS: usize, const NUM_WORDS: usize, F>(
    shared_state: &SharedState<NUM_THREADS, NUM_WORDS>,
    thread_index: ThreadIndex,
    thread_and_sequence: ThreadAndSequence,
    starting_word_num: WordNum,
    next_function: F,
) -> Result<(), KCasError>
    where F: FnOnce() -> Result<(), KCasError>
{
    match revert(shared_state, thread_index, thread_and_sequence, starting_word_num) {
        Ok(_) => {
            next_function()
        }
        Err(revert_error) => {
            match revert_error {
                RevertError::TargetAddressWasNotValidPointer(actual_value) => {
                    Err(KCasError::Fatal(FatalError::TargetAddressWasNotValidPointer(actual_value)))
                }
            }
        }
    }
}

pub(super) fn help_revert_and_transition<const NUM_THREADS: usize, const NUM_WORDS: usize, F>(
    shared_state: &SharedState<NUM_THREADS, NUM_WORDS>,
    thread_index: ThreadIndex,
    thread_and_sequence: ThreadAndSequence,
    starting_word_num: WordNum,
    next_function: F,
) -> Result<(), HelpError>
    where F: FnOnce() -> Result<(), HelpError>
{
    match revert(shared_state, thread_index, thread_and_sequence, starting_word_num) {
        Ok(_) => {
            next_function()
        }
        Err(revert_error) => {
            match revert_error {
                RevertError::TargetAddressWasNotValidPointer(actual_value) => {
                    Err(HelpError::Fatal(FatalError::TargetAddressWasNotValidPointer(actual_value)))
                }
            }
        }
    }
}

pub(super) enum RevertError {
    TargetAddressWasNotValidPointer(WordNum),
}

pub(super) fn revert<const NUM_THREADS: usize, const NUM_WORDS: usize>(
    shared_state: &SharedState<NUM_THREADS, NUM_WORDS>,
    thread_index: ThreadIndex,
    thread_and_sequence: ThreadAndSequence,
    starting_word_num: WordNum
) -> Result<(), RevertError> {
    for word_num in (0..=starting_word_num).rev() {
        let target_address: &AtomicPtr<AtomicUsize> = &shared_state.target_addresses[thread_index][word_num];
        let target_address: *mut AtomicUsize = target_address.load(Ordering::Acquire);
        let target_reference: &AtomicUsize = unsafe {
            target_address.as_ref().ok_or_else(|| RevertError::TargetAddressWasNotValidPointer(word_num))?
        };

        // put the expected element back
        let expected_element: usize = shared_state.expected_elements[thread_index][word_num].load(Ordering::Acquire);

        // even if the target address did not contain `thread_and_sequence`, keep trying to revert
        // the remaining addresses
        _ = target_reference.compare_exchange(thread_and_sequence, expected_element, Ordering::AcqRel, Ordering::Acquire);
    }
    Ok(())
}
