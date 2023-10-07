use crate::aliases::{SequenceNum, ThreadAndSequence, ThreadIndex, WordNum};
use crate::err::{FatalError, KCasError};
use crate::kcas::revert::{help_revert_and_transition, revert_and_transition};
use crate::kcas::set::{help_set_and_transition, set_and_transition};
use crate::kcas::stage_change::{
    change_stage_and_transition,
    handle_concurrent_stage_and_sequence_verification_error_and_transition,
    help_change_stage_and_transition,
    help_handle_concurrent_stage_and_sequence_verification_error_and_transition,
    verify_stage_and_sequence_have_not_changed, StageAndSequenceVerificationError,
};
use crate::kcas::{
    combine_stage_and_sequence, extract_thread_from_thread_and_sequence, help_thread,
    reset_for_next_operation, HelpError, State,
};
use crate::stage::Stage;
use crate::sync::{AtomicPtr, AtomicUsize, Ordering};

pub(super) fn claim_and_transition<const NUM_THREADS: usize, const NUM_WORDS: usize>(
    shared_state: &State<NUM_THREADS, NUM_WORDS>,
    thread_index: ThreadIndex,
    sequence: SequenceNum,
    thread_and_sequence: ThreadAndSequence,
) -> Result<(), KCasError> {
    let claim_result: Result<(), ClaimError> =
        claim(shared_state, thread_index, sequence, thread_and_sequence);
    if claim_result.is_ok() {
        let stage_and_sequence: ThreadAndSequence =
            combine_stage_and_sequence(Stage::Claiming, sequence);
        return change_stage_and_transition(
            shared_state,
            thread_index,
            sequence,
            thread_and_sequence,
            stage_and_sequence,
            Stage::Claiming,
            Stage::Setting,
            || set_and_transition(shared_state, thread_index, sequence, thread_and_sequence),
        );
    }
    match claim_result.unwrap_err() {
        ClaimError::StageAndSequenceVerificationError {
            failed_word_num,
            error,
        } => handle_concurrent_stage_and_sequence_verification_error_and_transition(
            shared_state,
            thread_index,
            sequence,
            thread_and_sequence,
            Stage::Claiming,
            error,
        ),
        ClaimError::ValueWasNotExpectedValue {
            word_num,
            actual_value,
        } => change_stage_and_transition(
            shared_state,
            thread_index,
            sequence,
            thread_and_sequence,
            combine_stage_and_sequence(Stage::Claiming, sequence),
            Stage::Claiming,
            Stage::Reverting,
            || {
                revert_and_transition(
                    shared_state,
                    thread_index,
                    thread_and_sequence,
                    word_num,
                    || {
                        reset_for_next_operation(shared_state, thread_index, sequence);
                        Err(KCasError::ValueWasNotExpectedValue)
                    },
                )
            },
        ),
        ClaimError::TargetAddressWasNotValidPointer(failed_word_num) => Err(KCasError::Fatal(
            FatalError::TargetAddressWasNotValidPointer(failed_word_num),
        )),
        ClaimError::FatalErrorWhileHelping(fatal_error) => Err(KCasError::Fatal(fatal_error)),
    }
}

pub(super) fn help_claim_and_transition<const NUM_THREADS: usize, const NUM_WORDS: usize>(
    shared_state: &State<NUM_THREADS, NUM_WORDS>,
    thread_index: ThreadIndex,
    sequence: SequenceNum,
    thread_and_sequence: ThreadAndSequence,
) -> Result<(), HelpError> {
    let claim_result: Result<(), ClaimError> =
        claim(shared_state, thread_index, sequence, thread_and_sequence);
    if claim_result.is_ok() {
        let stage_and_sequence: ThreadAndSequence =
            combine_stage_and_sequence(Stage::Claiming, sequence);
        return help_change_stage_and_transition(
            shared_state,
            thread_index,
            sequence,
            thread_and_sequence,
            stage_and_sequence,
            Stage::Claiming,
            Stage::Setting,
            || help_set_and_transition(shared_state, thread_index, sequence, thread_and_sequence),
        );
    }
    match claim_result.unwrap_err() {
        ClaimError::StageAndSequenceVerificationError {
            failed_word_num,
            error,
        } => help_handle_concurrent_stage_and_sequence_verification_error_and_transition(
            shared_state,
            thread_index,
            sequence,
            thread_and_sequence,
            Stage::Claiming,
            error,
        ),
        ClaimError::ValueWasNotExpectedValue {
            word_num,
            actual_value,
        } => help_change_stage_and_transition(
            shared_state,
            thread_index,
            sequence,
            thread_and_sequence,
            combine_stage_and_sequence(Stage::Claiming, sequence),
            Stage::Claiming,
            Stage::Reverting,
            || {
                help_revert_and_transition(
                    shared_state,
                    thread_index,
                    thread_and_sequence,
                    word_num,
                    || {
                        help_change_stage_and_transition(
                            shared_state,
                            thread_index,
                            sequence,
                            thread_and_sequence,
                            combine_stage_and_sequence(Stage::Reverting, sequence),
                            Stage::Reverting,
                            Stage::Reverted,
                            || Ok(()),
                        )
                    },
                )
            },
        ),
        ClaimError::TargetAddressWasNotValidPointer(failed_word_num) => Err(HelpError::Fatal(
            FatalError::TargetAddressWasNotValidPointer(failed_word_num),
        )),
        ClaimError::FatalErrorWhileHelping(fatal_error) => Err(HelpError::Fatal(fatal_error)),
    }
}

enum ClaimError {
    StageAndSequenceVerificationError {
        failed_word_num: WordNum,
        error: StageAndSequenceVerificationError,
    },
    ValueWasNotExpectedValue {
        word_num: WordNum,
        actual_value: usize,
    },
    TargetAddressWasNotValidPointer(WordNum),
    FatalErrorWhileHelping(FatalError),
}

fn claim<const NUM_THREADS: usize, const NUM_WORDS: usize>(
    shared_state: &State<NUM_THREADS, NUM_WORDS>,
    thread_index: ThreadIndex,
    sequence: SequenceNum,
    thread_and_sequence: ThreadAndSequence,
) -> Result<(), ClaimError> {
    let mut word_num: WordNum = 0;
    while word_num < NUM_WORDS {
        let target_address: &AtomicPtr<AtomicUsize> =
            &shared_state.target_addresses[thread_index][word_num];
        let target_address: *mut AtomicUsize = target_address.load(Ordering::Acquire);
        let target_reference: &AtomicUsize = unsafe {
            target_address
                .as_ref()
                .ok_or_else(|| ClaimError::TargetAddressWasNotValidPointer(word_num))?
        };

        let expected_element: usize =
            shared_state.expected_elements[thread_index][word_num].load(Ordering::Acquire);

        match target_reference.compare_exchange(
            expected_element,
            thread_and_sequence,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => {
                word_num += 1;
            }
            Err(actual_value) => {
                if actual_value == thread_and_sequence {
                    word_num += 1;
                    continue;
                }

                if let Err(error) = verify_stage_and_sequence_have_not_changed(
                    shared_state,
                    thread_index,
                    Stage::Claiming,
                    sequence,
                ) {
                    return Err(ClaimError::StageAndSequenceVerificationError {
                        failed_word_num: word_num,
                        error,
                    });
                }

                match extract_thread_from_thread_and_sequence(
                    actual_value,
                    shared_state.num_threads_bit_length,
                ) {
                    0 => {
                        return Err(ClaimError::ValueWasNotExpectedValue {
                            word_num,
                            actual_value,
                        })
                    }
                    other_thread_id => {
                        // help thread, then continue
                        match help_thread(shared_state, actual_value) {
                            Ok(_) => {
                                // try this iteration again
                            }
                            Err(help_error) => {
                                match help_error {
                                    HelpError::SequenceChangedWhileHelping
                                    | HelpError::HelpeeStageIsAlreadyTerminal(_) => {
                                        // try this iteration again
                                    }
                                    HelpError::Fatal(fatal_error) => {
                                        return Err(ClaimError::FatalErrorWhileHelping(
                                            fatal_error,
                                        ));
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    Ok(())
}
