use core::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};
use crate::aliases::{SequenceNum, StageAndSequence, ThreadAndSequence, ThreadId, ThreadIndex, WordNum};
use crate::err::{ConcurrentChangeError, FatalError, KCasError, StageOutOfBoundsError};
use crate::err::KCasError::FatalInternalError;
use crate::stage::{Stage, STATUS_BIT_LENGTH};

#[derive(Debug, Eq, PartialEq)]
pub struct KCasWord<'a> {
    target_address: &'a mut usize,
    expected_element: usize,
    desired_element: usize,
}

impl<'a> KCasWord<'a> {
    pub fn new(target_address: &'a mut usize, expected_element: usize, desired_element: usize) -> Self {
        Self {
            target_address, expected_element, desired_element,
        }
    }
}

pub struct SharedState<const NUM_THREADS: usize, const NUM_WORDS: usize> {
    /// The binary bit length of the number of threads. For example, if NUM_THREADS is 5, then the
    /// bit length is 3, because 5 in binary is 101, which has 3 bits.
    num_threads_bit_length: usize,

    /// A mask to help us extract the sequence number out of a number which contains both a thread
    /// id (in the most significant bits) and a sequence number (in the least significant bits).
    ///
    /// This mask may be different for different depending on the number of threads. Store this to
    /// guarantee it is calculated just once.
    sequence_mask_for_thread_and_sequence: usize,

    /// Each thread has its own sub-array here where it stores destructured CasRows before it
    /// begins performing multi-word CAS.
    target_addresses: [[AtomicPtr<AtomicUsize>; NUM_WORDS]; NUM_THREADS],
    expected_elements: [[AtomicUsize; NUM_WORDS]; NUM_THREADS],
    desired_elements: [[AtomicUsize; NUM_WORDS]; NUM_THREADS],

    /// Each thread maintains a stage and sequence number. The 3 most significant bits are
    /// reserved for serialization of the stage. The rest of the bits are the sequence number.
    stage_and_sequence_numbers: [AtomicUsize; NUM_THREADS],
}

impl<const NUM_THREADS: usize, const NUM_WORDS: usize> SharedState<NUM_THREADS, NUM_WORDS> {
    fn new() -> Self {
        Self {
            num_threads_bit_length: get_bit_length(NUM_THREADS),
            sequence_mask_for_thread_and_sequence: get_sequence_mask_for_thread_and_sequence(NUM_THREADS),
            target_addresses: core::array::from_fn(|_| core::array::from_fn(|_| AtomicPtr::default())),
            expected_elements: core::array::from_fn(|_| core::array::from_fn(|_| AtomicUsize::default())),
            desired_elements: core::array::from_fn(|_| core::array::from_fn(|_| AtomicUsize::default())),
            stage_and_sequence_numbers: core::array::from_fn(|_| AtomicUsize::default()),
        }
    }
}

/// The mask to AND with a combined stage and sequence number in order to extract the sequence.
const SEQUENCE_MASK_FOR_STATUS_AND_SEQUENCE: usize = {
    let num_sequence_bits: usize = (usize::BITS as usize - STATUS_BIT_LENGTH);
    !(0b11 << num_sequence_bits)
};

/// Obtain the bit length of a number. For example, if `number` is 5, then the bit length is 3
/// because the binary representation of 5, 101, consists of 3 bits.
fn get_bit_length(number: usize) -> usize {
    for i in 0..usize::BITS as usize {
        if number >> i == 0usize {
            return i;
        }
    }
    usize::BITS as usize
}

/// Construct a mask to help us extract the sequence number out of a number which
/// contains both a thread id (in the most significant bits) and a sequence number (in the least
/// significant bits).
fn get_sequence_mask_for_thread_and_sequence(num_threads_bit_length: usize) -> usize {
    let num_sequence_bits: usize = usize::BITS as usize - num_threads_bit_length;
    !(usize::MAX << num_sequence_bits)
}

/// Construct a number containing both a `Stage` and a sequence number.
///
/// The `Stage` takes up the [STATUS_BIT_LENGTH] most significant bits, and the sequence number
/// takes up the rest.
fn combine_stage_and_sequence(stage: Stage, sequence: SequenceNum) -> StageAndSequence {
    (stage as usize) << (usize::BITS as usize - STATUS_BIT_LENGTH) | sequence
}

/// Extract the Stage out of a number which holds both a stage and a sequence number.
/// The `Stage` is obtained from the [STATUS_BIT_LENGTH] most significant bits.
fn extract_stage_from_stage_and_sequence(
    stage_and_sequence: StageAndSequence
) -> Result<Stage, StageOutOfBoundsError> {
    let stage_as_num: usize = stage_and_sequence >> (usize::BITS as usize - STATUS_BIT_LENGTH);
    Stage::try_from(stage_as_num)
}

/// Extract the sequence number out of a number which holds both a stage and a sequence number.
/// The sequence number is obtained from the rest of the bits besides the [STATUS_BIT_LENGTH] most significant.
fn extract_sequence_from_stage_and_sequence(
    stage_and_sequence: StageAndSequence
) -> SequenceNum {
    stage_and_sequence & SEQUENCE_MASK_FOR_STATUS_AND_SEQUENCE
}

fn combine_thread_id_and_sequence(
    thread_id: ThreadId,
    sequence: SequenceNum,
    num_threads_bit_length: usize
) -> ThreadAndSequence {
    thread_id << (usize::BITS as usize - num_threads_bit_length) | sequence
}

fn extract_thread_from_thread_and_sequence(
    thread_and_sequence: ThreadAndSequence,
    num_threads_bit_length: usize
) -> ThreadId {
    thread_and_sequence >> (usize::BITS as usize - num_threads_bit_length)
}

fn extract_sequence_from_thread_and_sequence(
    thread_and_sequence: ThreadAndSequence,
    sequence_mask_for_thread_and_sequence: usize
) -> SequenceNum {
    thread_and_sequence & sequence_mask_for_thread_and_sequence
}

fn verify_stage_and_sequence_have_not_changed<const NUM_THREADS: usize, const NUM_WORDS: usize>(
    thread_state: &SharedState<NUM_THREADS, NUM_WORDS>,
    thread_index: usize,
    expected_stage: Stage,
    expected_sequence: SequenceNum,
) -> Result<(), StageAndSequenceVerificationError> {
    let actual_stage_and_sequence: StageAndSequence = thread_state.stage_and_sequence_numbers[thread_index].load(Ordering::Acquire);
    let actual_sequence: SequenceNum = extract_sequence_from_stage_and_sequence(actual_stage_and_sequence);
    let actual_stage: Stage = extract_stage_from_stage_and_sequence(actual_stage_and_sequence)?;

    if actual_sequence != expected_sequence {
        return Err(StageAndSequenceVerificationError::SequenceChanged{ actual_sequence });
    }
    if actual_stage != expected_stage {
        return Err(StageAndSequenceVerificationError::StageChanged { actual_stage });
    }
    Ok(())
}

enum StageAndSequenceVerificationError {
    StageChanged { actual_stage: Stage },
    SequenceChanged { actual_sequence: SequenceNum },
    StageOutOfBounds { actual_stage_number: usize },
}

impl From<StageOutOfBoundsError> for StageAndSequenceVerificationError {
    fn from(stage_out_of_bounds_error: StageOutOfBoundsError) -> Self {
        Self::StageOutOfBounds { actual_stage_number: stage_out_of_bounds_error.0 }
    }
}

enum ChangeStageFromClaimingToSettingOutput {
    StageChangedToSetting { sequence: SequenceNum },
    StageAndSequenceVerificationError { sequence: SequenceNum, error: StageAndSequenceVerificationError },
}

enum ChangeStageFromClaimingToRevertingOutput {
    StageChangedToReverting { sequence: SequenceNum },
    StageAndSequenceVerificationError { sequence: SequenceNum, error: StageAndSequenceVerificationError },
}

enum ClaimOutput {
    Claimed { sequence: SequenceNum, claimed_word: WordNum },
    StageAndSequenceVerificationError { sequence: SequenceNum, current_word_num: WordNum, error: StageAndSequenceVerificationError },
    ActualValueDidNotMatchExpectedValue { sequence: SequenceNum, word_num: WordNum, actual_value: usize },
    ValueWasAnotherThreadAndSequence { sequence: SequenceNum, word_num: WordNum, other_thread_id: ThreadId, other_sequence: SequenceNum },
    TargetAddressWasNotValidPointer { sequence: SequenceNum, word_num: WordNum },
}

enum SetOutput {
    Set { sequence: SequenceNum, set_word: WordNum },
    StageAndSequenceVerificationError { sequence: SequenceNum, current_word_num: WordNum, error: StageAndSequenceVerificationError },
    ValueWasNotClaimMarker { sequence: SequenceNum, word_num: WordNum, actual_value: usize },
    TargetAddressWasNotValidPointer { sequence: SequenceNum, word_num: WordNum },
}

enum RevertOutput {
    Reverted { sequence: SequenceNum, reverted_word: WordNum, operation_result: Result<(), KCasError> },
    StageAndSequenceVerificationError { sequence: SequenceNum, current_word_num: WordNum, error: StageAndSequenceVerificationError },
    TargetAddressWasNotValidPointer { sequence: SequenceNum, word_num: WordNum },
}

enum HelpClaimResult {
    Claimed { sequence: SequenceNum, claimed_word: WordNum },
    StageAndSequenceVerificationError { sequence: SequenceNum, current_word_num: WordNum, error: StageAndSequenceVerificationError },
    ActualValueDidNotMatchExpectedValue { sequence: SequenceNum, word_num: WordNum, actual_value: usize },
    ValueWasAnotherThreadAndSequence { sequence: SequenceNum, word_num: WordNum, other_thread_id: ThreadId, other_sequence: SequenceNum },
    TargetAddressWasNotValidPointer { sequence: SequenceNum, word_num: WordNum },
}

enum HelpChangeStageFromClaimingToSettingResult {

}

enum HelpChangeStageFromClaimingToRevertingResult {

}

enum HelpSetResult {

}

enum HelpRevertResult {

}

enum ClearStateResult {
    StateCleared(Result<(), KCasError>)
}

enum DetermineHowToHelpThreadOutput {
    SequenceChanged { original_operation_state: OriginalOperationState, actual_sequence: SequenceNum },
    StageOutOfBounds { actual_stage_number: usize },

    OperationWasInactive { thread_id: ThreadId, sequence: SequenceNum },
    HelpClaim { original_operation_state: OriginalOperationState, thread_id: ThreadId, sequence: SequenceNum, },
    HelpSet { original_operation_state: OriginalOperationState, thread_id: ThreadId, sequence: SequenceNum, },
    HelpRevert { original_operation_state: OriginalOperationState, thread_id: ThreadId, sequence: SequenceNum, },
    NothingToHelp { original_operation_state: OriginalOperationState, },
}

struct OriginalOperationState {
    thread_id: ThreadId,
    sequence: SequenceNum,
    word_num: WordNum,
}

impl OriginalOperationState {
    fn new(thread_id: ThreadId, sequence: SequenceNum, word_num: WordNum) -> Self {
        Self { thread_id, sequence, word_num }
    }
}

enum DetermineHowToResumeOriginalOperationResult {

}

enum StepOutput {
    InitializeSharedState(SequenceNum),

    Claim(ClaimOutput),
    ChangeStageFromClaimingToSetting(ChangeStageFromClaimingToSettingOutput),
    ChangeStageFromClaimingToReverting(ChangeStageFromClaimingToRevertingOutput),
    Set(SetOutput),
    Revert(RevertOutput),

    DetermineHowToHelpThread(DetermineHowToHelpThreadOutput),

    ClearState(ClearStateResult),
}

enum HelpStepResult {
    HelpClaim(HelpClaimResult),
    HelpChangeStageFromClaimingToSetting(HelpChangeStageFromClaimingToSettingResult),
    HelpChangeStageFromClaimingToReverting(HelpChangeStageFromClaimingToRevertingResult),
    HelpSet(HelpSetResult),
    HelpRevert(HelpRevertResult),
    DetermineHowToResumeOriginalOperation(DetermineHowToResumeOriginalOperationResult)
}

enum FatalStepError {
    AttemptedToTransitionFromTerminalStep,
}

fn kcasv2<const NUM_THREADS: usize, const NUM_WORDS: usize>(
    shared_state: &SharedState<NUM_THREADS, NUM_WORDS>,
    thread_id: ThreadId,
    kcas_words: [KCasWord; NUM_WORDS]
) -> Result<(), KCasError> {
    let mut step_result: StepOutput = initialize_shared_state(shared_state, thread_id, kcas_words);
    loop {

    }
}

fn handle_stage_and_sequence_verification_error<const NUM_THREADS: usize, const NUM_WORDS: usize>(
    shared_state: &SharedState<NUM_THREADS, NUM_WORDS>,
    thread_id: ThreadId,
    sequence: SequenceNum,
    first_revert_word_num: WordNum,
    stage_and_sequence_verification_error: StageAndSequenceVerificationError,
) -> StepOutput {
    match stage_and_sequence_verification_error {
        StageAndSequenceVerificationError::StageChanged { actual_stage } => {
            match actual_stage {
                // It should not be possible to go backwards from `Claiming` to `Inactive`
                Stage::Inactive => {
                    let error = KCasError::FatalInternalError(FatalError::IllegalStageTransition { original_stage: Stage::Claiming, next_stage: Stage::Inactive });
                    StepOutput::Revert(revert(shared_state, thread_id, sequence, first_revert_word_num, Err(error)))
                }
                // We should not get here if the stage is still `Claiming` because the stage was already `Claiming`
                Stage::Claiming => {
                    let error = KCasError::FatalInternalError(FatalError::IllegalStageTransition { original_stage: Stage::Claiming, next_stage: Stage::Claiming });
                    StepOutput::Revert(revert(shared_state, thread_id, sequence, first_revert_word_num, Err(error)))
                }
                // Another thread succeeded in claiming all addresses and it advanced the stage to Setting.
                // This thread should start setting, too.
                Stage::Setting => {
                    StepOutput::Set(set(shared_state, thread_id, sequence, 0))
                }
                // Another thread encountered a CAS failure and advanced the stage to
                // Reverting. This thread should start reverting, too.
                Stage::Reverting => {
                    StepOutput::Revert(revert(shared_state, thread_id, sequence, NUM_WORDS, Err(KCasError::ValueWasNotExpectedValue)))
                }
                // Another thread succeeded in claiming all addresses, then succeeded
                // in setting all addresses to the desired values. It is possible
                // that this current thread claimed addresses which were already set
                // by that other thread, so try to revert such claims
                // before returning success
                Stage::Successful => {
                    StepOutput::Revert(revert(shared_state, thread_id, sequence, first_revert_word_num, Ok(())))
                }
                // Another thread encountered a CAS failure, advanced the stage to
                // Reverting and reverted all fields. This thread should revert, as
                // well, to release any addresses it claimed after the other thread
                // already reverted.
                Stage::Reverted => {
                    StepOutput::Revert(revert(shared_state, thread_id, sequence, first_revert_word_num, Err(KCasError::ValueWasNotExpectedValue)))
                }
            }
        }
        StageAndSequenceVerificationError::SequenceChanged { actual_sequence } => {
            // a thread's sequence number changed while it was claiming target addresses for its own operation, which should not be possible
            // because a thread is only allowed to change its own sequence number, and a thread
            // should not try to change a sequence number while claiming. Revert
            // to release any claimed addresses.
            StepOutput::Revert(
                revert(
                    shared_state, thread_id, sequence, first_revert_word_num,
                    Err(KCasError::FatalInternalError(FatalError::ThreadSequenceModifiedByAnotherThread { thread_id }))
                )
            )
        }
        StageAndSequenceVerificationError::StageOutOfBounds { actual_stage_number } => {
            StepOutput::Revert(
                revert(
                    shared_state, thread_id, sequence, first_revert_word_num,
                    Err(KCasError::FatalInternalError(FatalError::OutOfBoundsStage(actual_stage_number)))
                )
            )
        }
    }
}

// fn help_another_thread_perform_step<const NUM_THREADS: usize, const NUM_WORDS: usize>(
//     shared_state: &SharedState<NUM_THREADS, NUM_WORDS>,
//     thread_id: ThreadId,
//     step_result: HelpStepResult,
// ) -> Result<HelpStepResult, FatalStepError> {
// }

fn perform_next_step<const NUM_THREADS: usize, const NUM_WORDS: usize>(
    shared_state: &SharedState<NUM_THREADS, NUM_WORDS>,
    thread_id: ThreadId,
    step_result: StepOutput,
) -> Result<StepOutput, FatalStepError> {
    match step_result {
        StepOutput::InitializeSharedState(sequence) => {
            Ok(StepOutput::Claim(claim(shared_state, thread_id, sequence, 0)))
        }
        StepOutput::Claim(claim_result) => {
            match claim_result {
                ClaimOutput::Claimed { sequence, claimed_word } => {
                    if claimed_word < NUM_WORDS {
                        Ok(StepOutput::Claim(claim(shared_state, thread_id, sequence, claimed_word + 1)))
                    } else {
                        Ok(StepOutput::Set(set(shared_state, thread_id, sequence, 0)))
                    }
                }
                ClaimOutput::StageAndSequenceVerificationError { sequence, current_word_num, error } => {
                    Ok(handle_stage_and_sequence_verification_error(shared_state, thread_id, sequence, current_word_num, error))
                }
                ClaimOutput::ActualValueDidNotMatchExpectedValue { sequence, word_num, actual_value } => {
                    Ok(StepOutput::Revert(revert(shared_state, thread_id, sequence, NUM_WORDS, Err(KCasError::ValueWasNotExpectedValue))))
                }
                ClaimOutput::ValueWasAnotherThreadAndSequence { sequence, word_num, other_thread_id, other_sequence } => {
                    // Another thread claimed the target address before we did. Help them to completion.
                    let original_operation_state: OriginalOperationState = OriginalOperationState::new(thread_id, sequence, word_num);
                    Ok(StepOutput::DetermineHowToHelpThread(determine_how_to_help_thread(shared_state, original_operation_state, other_thread_id, other_sequence)))
                }
                ClaimOutput::TargetAddressWasNotValidPointer { sequence, word_num } => {
                    // Something happened to the target address and it's no longer a valid pointer.
                    // Try to revert all the addresses before this which we claimed.
                    Ok(StepOutput::Revert(revert(shared_state, thread_id, sequence, NUM_WORDS, Err(KCasError::FatalInternalError(FatalError::InvalidPointer)))))
                }
            }
        }
        StepOutput::ChangeStageFromClaimingToSetting(change_stage_from_claiming_to_setting_result) => {
            match change_stage_from_claiming_to_setting_result {
                ChangeStageFromClaimingToSettingOutput::StageAndSequenceVerificationError { sequence, error } => {
                    Ok(handle_stage_and_sequence_verification_error(shared_state, thread_id, sequence, NUM_WORDS, error))
                }
                ChangeStageFromClaimingToSettingOutput::StageChangedToSetting { sequence } => {
                    Ok(StepOutput::Set(set(shared_state, thread_id, sequence, 0)))
                }
            }
        }
        StepOutput::ChangeStageFromClaimingToReverting(change_stage_from_claiming_to_reverting_result) => {
            match change_stage_from_claiming_to_reverting_result {
                ChangeStageFromClaimingToRevertingOutput::StageAndSequenceVerificationError { sequence, error } => {
                    Ok(handle_stage_and_sequence_verification_error(shared_state, thread_id, sequence, NUM_WORDS, error))
                }
                ChangeStageFromClaimingToRevertingOutput::StageChangedToReverting { sequence } => {
                    Ok(StepOutput::Revert(revert(shared_state, thread_id, sequence, NUM_WORDS, Err(KCasError::ValueWasNotExpectedValue))))
                }
            }
        }
        StepOutput::Set(set_result) => {
            match set_result {
                SetOutput::Set { sequence, set_word } => {
                    if set_word < NUM_WORDS {
                        Ok(StepOutput::Set(set(shared_state, thread_id, sequence, set_word + 1)))
                    } else {
                        Ok(StepOutput::ClearState(clear_state(shared_state, thread_id, sequence, Ok(()))))
                    }
                }
                SetOutput::StageAndSequenceVerificationError { sequence, current_word_num, error } => {
                    Ok(handle_stage_and_sequence_verification_error(shared_state, thread_id, sequence, current_word_num, error))
                }
                SetOutput::ValueWasNotClaimMarker { sequence, word_num, actual_value } => {
                    Ok(StepOutput::Revert(
                        revert(
                            shared_state, thread_id, sequence, NUM_WORDS,
                            Err(KCasError::FatalInternalError(FatalError::IllegalState))
                        )
                    ))
                }
                SetOutput::TargetAddressWasNotValidPointer { sequence, word_num } => {
                    Ok(StepOutput::Revert(
                        revert(
                            shared_state, thread_id, sequence, NUM_WORDS,
                            Err(KCasError::FatalInternalError(FatalError::InvalidPointer))
                        )
                    ))
                }
            }
        }
        StepOutput::Revert(revert_result) => {
            match revert_result {
                RevertOutput::Reverted { sequence, reverted_word, operation_result } => {
                    if reverted_word > 0 {
                        Ok(StepOutput::Revert(revert(shared_state, thread_id, sequence, reverted_word - 1, operation_result)))
                    } else {
                        Ok(StepOutput::ClearState(clear_state(shared_state, thread_id, sequence, operation_result)))
                    }
                }
                RevertOutput::TargetAddressWasNotValidPointer { sequence, word_num } => {
                    // a target address was invalid. Skip the invalid address and keep trying to finish the revert
                    let invalid_pointer_result = Err(KCasError::FatalInternalError(FatalError::InvalidPointer));
                    if word_num > 0 {
                        Ok(StepOutput::Revert(revert(shared_state, thread_id, sequence, word_num - 1, invalid_pointer_result)))
                    } else {
                        Ok(StepOutput::ClearState(clear_state(shared_state, thread_id, sequence, invalid_pointer_result)))
                    }
                }
                RevertOutput::StageAndSequenceVerificationError { sequence, current_word_num, error } => {
                    Ok(handle_stage_and_sequence_verification_error(shared_state, thread_id, sequence, current_word_num, error))
                }
            }
        }
        StepOutput::DetermineHowToHelpThread(_) => {
            // match start_helping_thread_result {
            //     DetermineHowThreadNeedsHelpResult::SequenceChanged { original_operation_state, actual_sequence } => {
            //         // if the sequence changed, we should try the original operation again
            //         StepResult::Claim(claim(shared_state, original_operation_state.thread_id, original_operation_state.sequence, original_operation_state.word_num))
            //     }
            //     DetermineHowThreadNeedsHelpResult::StageOutOfBounds { actual_stage_number } => {
            //         // the thread we were trying to help has an out of stage bound. This is undefined
            //         // behavior, so revert and return an error
            //         StepResult::Revert(revert(shared_state,  original_operation_state.thread_id, original_operation_state.sequence, word_num - 1, invalid_pointer_result))
            //     }
            //     DetermineHowThreadNeedsHelpResult::OperationWasInactive { .. } => {}
            //     DetermineHowThreadNeedsHelpResult::HelpClaim { .. } => {}
            //     DetermineHowThreadNeedsHelpResult::HelpSet { .. } => {}
            //     DetermineHowThreadNeedsHelpResult::HelpRevert { .. } => {}
            //     DetermineHowThreadNeedsHelpResult::NothingToHelp { .. } => {}
            // }
            Err(FatalStepError::AttemptedToTransitionFromTerminalStep)
        }
        StepOutput::ClearState(_) => {
            Err(FatalStepError::AttemptedToTransitionFromTerminalStep)
        }
    }
}

fn help_claim<const NUM_THREADS: usize, const NUM_WORDS: usize>(
    shared_state: &SharedState<NUM_THREADS, NUM_WORDS>,
    thread_id: ThreadId,
    sequence: SequenceNum,
    word_num: WordNum,
) -> HelpClaimResult {
    let thread_index: usize = thread_id - 1;
    let thread_and_sequence: ThreadAndSequence = combine_thread_id_and_sequence(thread_id, sequence, shared_state.num_threads_bit_length);

    let target_address: &AtomicPtr<AtomicUsize> = &shared_state.target_addresses[thread_index][word_num];
    let target_address: *mut AtomicUsize = target_address.load(Ordering::Acquire);
    let target_reference: &AtomicUsize = match unsafe { target_address.as_ref() } {
        None => return HelpClaimResult::TargetAddressWasNotValidPointer { sequence, word_num },
        Some(target_reference) => target_reference
    };

    let expected_element: usize = shared_state.expected_elements[thread_index][word_num].load(Ordering::Acquire);

    match target_reference.compare_exchange(expected_element, thread_and_sequence, Ordering::AcqRel, Ordering::Acquire) {
        Ok(_) => HelpClaimResult::Claimed { sequence, claimed_word: word_num },
        Err(actual_value) => {
            match verify_stage_and_sequence_have_not_changed(shared_state, thread_index, Stage::Claiming, sequence) {
                Ok(_) => {},
                Err(stage_and_sequence_verification_error) => {
                    return HelpClaimResult::StageAndSequenceVerificationError { sequence, current_word_num: word_num, error: stage_and_sequence_verification_error };
                }
            }

            if actual_value == thread_and_sequence {
                return HelpClaimResult::Claimed { sequence, claimed_word: word_num };
            }
            let possibly_other_thread_id: ThreadId = extract_thread_from_thread_and_sequence(thread_and_sequence, shared_state.num_threads_bit_length);
            if possibly_other_thread_id != 0 {
                let other_sequence: SequenceNum = extract_sequence_from_thread_and_sequence(thread_and_sequence, shared_state.sequence_mask_for_thread_and_sequence);
                return HelpClaimResult::ValueWasAnotherThreadAndSequence { sequence, word_num, other_thread_id: possibly_other_thread_id, other_sequence };
            }
            HelpClaimResult::ActualValueDidNotMatchExpectedValue { sequence, word_num, actual_value }
        }
    }
}

fn claim<const NUM_THREADS: usize, const NUM_WORDS: usize>(
    shared_state: &SharedState<NUM_THREADS, NUM_WORDS>,
    thread_id: ThreadId,
    sequence: SequenceNum,
    word_num: WordNum,
) -> ClaimOutput {
    let thread_index: usize = thread_id - 1;
    let thread_and_sequence: ThreadAndSequence = combine_thread_id_and_sequence(thread_id, sequence, shared_state.num_threads_bit_length);

    let target_address: &AtomicPtr<AtomicUsize> = &shared_state.target_addresses[thread_index][word_num];
    let target_address: *mut AtomicUsize = target_address.load(Ordering::Acquire);
    let target_reference: &AtomicUsize = match unsafe { target_address.as_ref() } {
        None => return ClaimOutput::TargetAddressWasNotValidPointer { sequence, word_num },
        Some(target_reference) => target_reference
    };

    let expected_element: usize = shared_state.expected_elements[thread_index][word_num].load(Ordering::Acquire);

    match target_reference.compare_exchange(expected_element, thread_and_sequence, Ordering::AcqRel, Ordering::Acquire) {
        Ok(_) => ClaimOutput::Claimed { sequence, claimed_word: word_num },
        Err(actual_value) => {
            match verify_stage_and_sequence_have_not_changed(shared_state, thread_index, Stage::Claiming, sequence) {
                Ok(_) => {},
                Err(stage_and_sequence_verification_error) => {
                    return ClaimOutput::StageAndSequenceVerificationError { sequence, current_word_num: word_num, error: stage_and_sequence_verification_error };
                }
            }

            if actual_value == thread_and_sequence {
                return ClaimOutput::Claimed { sequence, claimed_word: word_num };
            }
            let possibly_other_thread_id: ThreadId = extract_thread_from_thread_and_sequence(thread_and_sequence, shared_state.num_threads_bit_length);
            if possibly_other_thread_id != 0 {
                let other_sequence: SequenceNum = extract_sequence_from_thread_and_sequence(thread_and_sequence, shared_state.sequence_mask_for_thread_and_sequence);
                return ClaimOutput::ValueWasAnotherThreadAndSequence { sequence, word_num, other_thread_id: possibly_other_thread_id, other_sequence };
            }
            ClaimOutput::ActualValueDidNotMatchExpectedValue { sequence, word_num, actual_value }
        }
    }
}

fn set<const NUM_THREADS: usize, const NUM_WORDS: usize>(
    shared_state: &SharedState<NUM_THREADS, NUM_WORDS>,
    thread_id: ThreadId,
    sequence: SequenceNum,
    word_num: WordNum,
) -> SetOutput {
    let thread_index: usize = thread_id - 1;
    let thread_and_sequence: ThreadAndSequence = combine_thread_id_and_sequence(thread_id, sequence, shared_state.num_threads_bit_length);

    let target_address: &AtomicPtr<AtomicUsize> = &shared_state.target_addresses[thread_index][word_num];
    let target_address: *mut AtomicUsize = target_address.load(Ordering::Acquire);
    let target_reference: &AtomicUsize = match unsafe { target_address.as_ref() } {
        None => return SetOutput::TargetAddressWasNotValidPointer { sequence, word_num },
        Some(target_reference) => target_reference
    };

    let desired_element: usize = shared_state.desired_elements[thread_index][word_num].load(Ordering::Acquire);

    match target_reference.compare_exchange(thread_and_sequence, desired_element, Ordering::AcqRel, Ordering::Acquire) {
        Ok(_) => SetOutput::Set { sequence, set_word: word_num },
        Err(actual_value) => {
            match verify_stage_and_sequence_have_not_changed(shared_state, thread_index, Stage::Claiming, sequence) {
                Ok(_) => {},
                Err(stage_and_sequence_verification_error) => {
                    return SetOutput::StageAndSequenceVerificationError { sequence, current_word_num: word_num, error: stage_and_sequence_verification_error };
                }
            }

            if actual_value == desired_element {
                return SetOutput::Set { sequence, set_word: word_num };
            }
            SetOutput::ValueWasNotClaimMarker { sequence, word_num, actual_value }
        }
    }
}

fn revert<const NUM_THREADS: usize, const NUM_WORDS: usize>(
    shared_state: &SharedState<NUM_THREADS, NUM_WORDS>,
    thread_id: ThreadId,
    sequence: SequenceNum,
    word_num: WordNum,
    operation_result: Result<(), KCasError>,
) -> RevertOutput {
    let thread_index: usize = thread_id - 1;
    let thread_and_sequence: ThreadAndSequence = combine_thread_id_and_sequence(thread_id, sequence, shared_state.num_threads_bit_length);

    let target_address: &AtomicPtr<AtomicUsize> = &shared_state.target_addresses[thread_index][word_num];
    let target_address: *mut AtomicUsize = target_address.load(Ordering::Acquire);
    let target_reference: &AtomicUsize = match unsafe { target_address.as_ref() } {
        None => return RevertOutput::TargetAddressWasNotValidPointer { sequence, word_num },
        Some(target_reference) => target_reference
    };

    let expected_element: usize = shared_state.expected_elements[thread_index][word_num].load(Ordering::Acquire);

    target_reference.compare_exchange(thread_and_sequence, expected_element, Ordering::AcqRel, Ordering::Acquire);
    RevertOutput::Reverted { sequence, reverted_word: word_num, operation_result }
}

fn determine_how_to_help_thread<const NUM_THREADS: usize, const NUM_WORDS: usize>(
    shared_state: &SharedState<NUM_THREADS, NUM_WORDS>,
    original_operation_state: OriginalOperationState,
    thread_id: ThreadId,
    sequence: SequenceNum,
) -> DetermineHowToHelpThreadOutput {
    let thread_index: ThreadIndex = thread_id - 1;
    let stage_and_sequence = shared_state.stage_and_sequence_numbers[thread_index].load(Ordering::Acquire);

    let actual_sequence: SequenceNum = extract_sequence_from_stage_and_sequence(stage_and_sequence);
    if actual_sequence != sequence {
        return DetermineHowToHelpThreadOutput::SequenceChanged { original_operation_state, actual_sequence };
    }

    let actual_stage: Stage = match extract_stage_from_stage_and_sequence(stage_and_sequence) {
        Ok(stage) => stage,
        Err(stage_out_of_bounds_error) => {
            return DetermineHowToHelpThreadOutput::StageOutOfBounds { actual_stage_number: stage_out_of_bounds_error.0 };
        }
    };

    match actual_stage {
        // cannot be inactive because an inactive operation cannot have claimed any addresses
        Stage::Inactive => {
            DetermineHowToHelpThreadOutput::OperationWasInactive { thread_id, sequence }
        }
        // we can help this thead
        Stage::Claiming => {
            DetermineHowToHelpThreadOutput::HelpClaim { original_operation_state, thread_id, sequence }
        }
        // and this thread
        Stage::Setting => {
            DetermineHowToHelpThreadOutput::HelpSet { original_operation_state, thread_id, sequence }
        }
        // and this thread
        Stage::Reverting => {
            DetermineHowToHelpThreadOutput::HelpRevert { original_operation_state, thread_id, sequence }
        }
        // thread has already been fully helped
        Stage::Successful => {
            DetermineHowToHelpThreadOutput::NothingToHelp { original_operation_state }
        }
        Stage::Reverted => {
            DetermineHowToHelpThreadOutput::NothingToHelp { original_operation_state }
        }
    }
}

fn clear_state<const NUM_THREADS: usize, const NUM_WORDS: usize>(
    kcas_state: &SharedState<NUM_THREADS, NUM_WORDS>,
    thread_id: ThreadId,
    sequence: SequenceNum,
    operation_result: Result<(), KCasError>,
) -> ClearStateResult {
    let thread_index: usize = thread_id - 1;
    let next_sequence: SequenceNum = sequence + 1;
    let next_stage_and_sequence: StageAndSequence = combine_stage_and_sequence(Stage::Inactive, next_sequence);
    kcas_state.stage_and_sequence_numbers[thread_index].store(next_stage_and_sequence, Ordering::Release);
    ClearStateResult::StateCleared(operation_result)
}

fn initialize_shared_state<const NUM_THREADS: usize, const NUM_WORDS: usize>(
    shared_state: &SharedState<NUM_THREADS, NUM_WORDS>,
    thread_id: ThreadId,
    kcas_words: [KCasWord; NUM_WORDS]
) -> StepOutput {
    // thread ids are 1-indexed. Otherwise, we would be unable to tell the difference between thread 0 and an element with no thread id attached.
    let thread_index: usize = thread_id - 1;

    // first, get this thread's state ready
    // there is no need for CAS anywhere here because it is not possible for other threads to be
    // helping this thread yet
    let mut kcas_words = kcas_words;
    for row_num in 0..kcas_words.len() {
        let cas_row: &mut KCasWord = &mut kcas_words[row_num];

        let target_address: *mut AtomicUsize = unsafe { cas_row.target_address as *mut usize as *mut AtomicUsize };
        shared_state.target_addresses[thread_index][row_num].store(target_address, Ordering::Release);

        shared_state.expected_elements[thread_index][row_num].store(cas_row.expected_element, Ordering::Release);
        shared_state.desired_elements[thread_index][row_num].store(cas_row.desired_element, Ordering::Release);
    }
    let original_stage_and_sequence: StageAndSequence = shared_state.stage_and_sequence_numbers[thread_index].load(Ordering::Acquire);
    let sequence: SequenceNum = extract_sequence_from_stage_and_sequence(original_stage_and_sequence);

    // now we are ready to start "acquiring" slots
    let acquiring_stage_and_sequence: StageAndSequence = combine_stage_and_sequence(Stage::Claiming, sequence);
    shared_state.stage_and_sequence_numbers[thread_index].store(acquiring_stage_and_sequence, Ordering::Release);

    StepOutput::InitializeSharedState(sequence)
}

#[cfg(test)]
mod tests {
    use crate::kcasv2::{kcasv2, KCasWord, SharedState};

    #[test]
    fn test() {
        let shared_state: SharedState<1, 3> = SharedState::new();

        let mut first_location: usize = 50;
        let mut second_location: usize = 70;
        let mut third_location: usize = 100;
        println!("Initial first_location: {first_location}");
        println!("Initial second_location: {second_location}");
        println!("Initial third_location: {third_location}");
        let kcas_words: [KCasWord; 3] = [
            KCasWord::new(&mut first_location, 50, 51),
            KCasWord::new(&mut second_location, 70, 71),
            KCasWord::new(&mut third_location, 100, 101),
        ];

        assert!(kcasv2(&shared_state, 1, kcas_words).is_ok());

        println!("After KCAS first_location: {first_location}");
        println!("After KCAS second_location: {second_location}");
        println!("After KCAS third_location: {third_location}");
    }
}