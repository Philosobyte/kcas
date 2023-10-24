use crate::err::{Error, FatalError};
use crate::kcas::stage_change::{
    handle_concurrent_stage_and_sequence_verification_error_and_transition,
    help_change_stage_and_transition,
    help_handle_concurrent_stage_and_sequence_verification_error_and_transition,
    verify_stage_and_sequence_have_not_changed, StageAndSequenceVerificationError,
};
use crate::kcas::{prepare_for_next_operation, HelpError, State};
use crate::sync::{AtomicPtr, AtomicUsize, Ordering};
use crate::types::{SequenceNum, Stage, ThreadAndSequence, ThreadIndex, WordNum};
use displaydoc::Display;

use tracing::{instrument, trace};

/// Swap out [ThreadAndSequence] markers for the desired values. Then, reset state in preparation
/// for the next operation.
#[instrument]
pub(super) fn set_and_transition<const NUM_THREADS: usize, const NUM_WORDS: usize>(
    shared_state: &State<NUM_THREADS, NUM_WORDS>,
    thread_index: ThreadIndex,
    sequence: SequenceNum,
    thread_and_sequence: ThreadAndSequence,
) -> Result<(), Error> {
    let set_result: Result<(), SetError> =
        set(shared_state, thread_index, sequence, thread_and_sequence);
    if set_result.is_ok() {
        prepare_for_next_operation(shared_state, thread_index, sequence);
        return Ok(());
    }
    match set_result.unwrap_err() {
        SetError::StageAndSequenceVerificationError { error, .. } => {
            handle_concurrent_stage_and_sequence_verification_error_and_transition(
                shared_state,
                thread_index,
                sequence,
                thread_and_sequence,
                Stage::Setting,
                error,
            )
        }
        SetError::TargetAddressWasNotValidPointer {
            word_num,
            target_address,
        } => Err(Error::Fatal(FatalError::TargetAddressWasNotValidPointer {
            word_num,
            target_address,
        })),
    }
}

/// Help another thread swap out [ThreadAndSequence] markers for desired values. Then, change the
/// stage to [enum@Stage::Successful]  to notify the originating thread that its operation completed
/// successfully.
#[instrument]
pub(super) fn help_set_and_transition<const NUM_THREADS: usize, const NUM_WORDS: usize>(
    shared_state: &State<NUM_THREADS, NUM_WORDS>,
    thread_index: ThreadIndex,
    sequence: SequenceNum,
    thread_and_sequence: ThreadAndSequence,
) -> Result<(), HelpError> {
    let set_result: Result<(), SetError> =
        set(shared_state, thread_index, sequence, thread_and_sequence);
    if set_result.is_ok() {
        return help_change_stage_and_transition(
            shared_state,
            thread_index,
            sequence,
            thread_and_sequence,
            Stage::Setting,
            Stage::Successful,
            || Ok(()),
        );
    }
    match set_result.unwrap_err() {
        SetError::StageAndSequenceVerificationError { error, .. } => {
            help_handle_concurrent_stage_and_sequence_verification_error_and_transition(
                shared_state,
                thread_index,
                sequence,
                thread_and_sequence,
                Stage::Setting,
                error,
            )
        }
        SetError::TargetAddressWasNotValidPointer {
            word_num,
            target_address,
        } => Err(HelpError::Fatal(
            FatalError::TargetAddressWasNotValidPointer {
                word_num,
                target_address,
            },
        )),
    }
}

#[derive(Debug, Display)]
enum SetError {
    /** The stage or sequence changed while attempting to swap the desired value into the target
       address at word {failed_word_num}: {error}
    */
    StageAndSequenceVerificationError {
        failed_word_num: WordNum,
        error: StageAndSequenceVerificationError,
    },
    /// The target address {target_address} at word number {word_num} was not a valid pointer
    TargetAddressWasNotValidPointer {
        word_num: WordNum,
        target_address: usize,
    },
}

/// Swap out [ThreadAndSequence] markers for desired values.
#[instrument]
fn set<const NUM_THREADS: usize, const NUM_WORDS: usize>(
    shared_state: &State<NUM_THREADS, NUM_WORDS>,
    thread_index: ThreadIndex,
    sequence: SequenceNum,
    thread_and_sequence: ThreadAndSequence,
) -> Result<(), SetError> {
    for word_num in 0..NUM_WORDS {
        let target_address_ptr: &AtomicPtr<AtomicUsize> =
            &shared_state.target_addresses[thread_index][word_num];
        let target_address_ptr: *mut AtomicUsize = target_address_ptr.load(Ordering::Acquire);

        let target_address: &AtomicUsize = unsafe { target_address_ptr.as_ref() }.ok_or(
            SetError::TargetAddressWasNotValidPointer {
                word_num,
                target_address: target_address_ptr as usize,
            },
        )?;

        let desired_value: usize =
            shared_state.desired_values[thread_index][word_num].load(Ordering::Acquire);

        trace!(
            "thread_index {thread_index}: CAS expected marker {} for desired value {} for word num {}",
            thread_and_sequence, desired_value, word_num
        );
        match target_address.compare_exchange(
            thread_and_sequence,
            desired_value,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => {
                trace!("thread_index {thread_index}: CAS succeeded")
            }
            Err(actual_value) => {
                trace!("thread_index {thread_index}: CAS failed with actual value {actual_value}");
                if let Err(error) = verify_stage_and_sequence_have_not_changed(
                    shared_state,
                    thread_index,
                    Stage::Setting,
                    sequence,
                ) {
                    return Err(SetError::StageAndSequenceVerificationError {
                        failed_word_num: word_num,
                        error,
                    });
                }

                if actual_value != desired_value {
                    continue;
                }
            }
        }
    }
    Ok(())
}
