use crate::aliases::{SequenceNum, ThreadAndSequence, ThreadIndex, WordNum};
use crate::err::{FatalError, KCasError};
use crate::kcas::{combine_stage_and_sequence, HelpError, reset_for_next_operation, SharedState};
use crate::kcas::stage_change::{handle_concurrent_stage_and_sequence_verification_error_and_transition, help_change_stage_and_transition, help_handle_concurrent_stage_and_sequence_verification_error_and_transition, StageAndSequenceVerificationError, verify_stage_and_sequence_have_not_changed};
use crate::stage::Stage;
use crate::sync::{AtomicPtr, AtomicUsize, Ordering};

pub(super) fn set_and_transition<const NUM_THREADS: usize, const NUM_WORDS: usize>(
    shared_state: &SharedState<NUM_THREADS, NUM_WORDS>,
    thread_index: ThreadIndex,
    sequence: SequenceNum,
    thread_and_sequence: ThreadAndSequence,
) -> Result<(), KCasError> {
    let set_result: Result<(), SetError> = set(shared_state, thread_index, sequence, thread_and_sequence);
    if set_result.is_ok() {
        reset_for_next_operation(shared_state, thread_index, sequence);
        return Ok(());
    }
    match set_result.unwrap_err() {
        SetError::StageAndSequenceVerificationError { failed_word_num, error } => {
            handle_concurrent_stage_and_sequence_verification_error_and_transition(shared_state, thread_index, sequence, thread_and_sequence, Stage::Setting, error)
        }
        SetError::TargetAddressWasNotValidPointer(word_num) => {
            Err(KCasError::Fatal(FatalError::TargetAddressWasNotValidPointer(word_num)))
        }
        SetError::ValueWasNotClaimMarker { word_num, actual_value } => {
            Err(KCasError::Fatal(FatalError::TriedToSetValueWhichWasNotClaimMarker { word_num, actual_value }))
        }
    }
}

pub(super) fn help_set_and_transition<const NUM_THREADS: usize, const NUM_WORDS: usize>(
    shared_state: &SharedState<NUM_THREADS, NUM_WORDS>,
    thread_index: ThreadIndex,
    sequence: SequenceNum,
    thread_and_sequence: ThreadAndSequence,
) -> Result<(), HelpError> {
    let set_result: Result<(), SetError> = set(shared_state, thread_index, sequence, thread_and_sequence);
    if set_result.is_ok() {
        return help_change_stage_and_transition(
            shared_state,
            thread_index,
            sequence,
            thread_and_sequence,
            combine_stage_and_sequence(Stage::Setting, sequence),
            Stage::Setting,
            Stage::Successful,
            || Ok(())
        );
    }
    match set_result.unwrap_err() {
        SetError::StageAndSequenceVerificationError { failed_word_num, error } => {
            help_handle_concurrent_stage_and_sequence_verification_error_and_transition(shared_state, thread_index, sequence, thread_and_sequence, Stage::Setting, error)
        }
        SetError::TargetAddressWasNotValidPointer(word_num) => {
            Err(HelpError::Fatal(FatalError::TargetAddressWasNotValidPointer(word_num)))
        }
        SetError::ValueWasNotClaimMarker { word_num, actual_value } => {
            Err(HelpError::Fatal(FatalError::TriedToSetValueWhichWasNotClaimMarker { word_num, actual_value }))
        }
    }
}

enum SetError {
    StageAndSequenceVerificationError { failed_word_num: WordNum, error: StageAndSequenceVerificationError },
    TargetAddressWasNotValidPointer(WordNum),
    ValueWasNotClaimMarker { word_num: WordNum, actual_value: usize },
}

fn set<const NUM_THREADS: usize, const NUM_WORDS: usize>(
    shared_state: &SharedState<NUM_THREADS, NUM_WORDS>,
    thread_index: ThreadIndex,
    sequence: SequenceNum,
    thread_and_sequence: ThreadAndSequence,
) -> Result<(), SetError> {
    for word_num in 0..NUM_WORDS {
        let target_address: &AtomicPtr<AtomicUsize> = &shared_state.target_addresses[thread_index][word_num];
        let target_address: *mut AtomicUsize = target_address.load(Ordering::Acquire);
        let target_reference: &AtomicUsize = unsafe {
            target_address.as_ref().ok_or_else(|| SetError::TargetAddressWasNotValidPointer(word_num))?
        };

        let desired_element: usize = shared_state.desired_elements[thread_index][word_num].load(Ordering::Acquire);

        match target_reference.compare_exchange(thread_and_sequence, desired_element, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => {}
            Err(actual_value) => {
                if let Err(error) = verify_stage_and_sequence_have_not_changed(shared_state, thread_index, Stage::Setting, sequence) {
                    return Err(SetError::StageAndSequenceVerificationError { failed_word_num: word_num, error });
                }

                if actual_value != desired_element {
                    return Err(SetError::ValueWasNotClaimMarker { word_num, actual_value })
                }
            }
        }
    }
    Ok(())
}

