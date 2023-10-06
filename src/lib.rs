#![cfg_attr(not(test), no_std)]

extern crate alloc;

use core::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};
use crate::err::{ClaimError, ConcurrentChangeError, FatalError, StageOutOfBoundsError, KCasError, RevertError, SetError};
use crate::err::KCasError::FatalInternalError;
use crate::stage::{Stage, STATUS_BIT_LENGTH};
use crate::aliases::{SequenceNum, StageAndSequence, ThreadAndSequence, ThreadId};

#[cfg(test)]
use std::sync::mpsc::channel;

mod err;
mod stage;
mod aliases;
mod kcasv2;
mod kcasv3;
mod sync;

/// The components for a single CAS operation.
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

pub struct KCasState<const NUM_THREADS: usize, const NUM_WORDS: usize> {
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

impl<const NUM_THREADS: usize, const NUM_WORDS: usize> KCasState<NUM_THREADS, NUM_WORDS> {
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
    thread_state: &KCasState<NUM_THREADS, NUM_WORDS>,
    current_word_num: usize,
    thread_index: usize,
    expected_stage: Stage,
    expected_sequence: SequenceNum,
) -> Result<(), ConcurrentChangeError> {
    let current_stage_and_sequence: StageAndSequence = thread_state.stage_and_sequence_numbers[thread_index].load(Ordering::Acquire);
    let current_sequence: SequenceNum = extract_sequence_from_stage_and_sequence(current_stage_and_sequence);
    let current_stage: Stage = extract_stage_from_stage_and_sequence(current_stage_and_sequence)?;

    if current_sequence != expected_sequence {
        return Err(ConcurrentChangeError::SequenceChanged { current_word_num, current_sequence });
    }
    if current_stage != expected_stage {
        return Err(ConcurrentChangeError::StageChanged { current_word_num, current_stage });
    }
    Ok(())
}

fn kcas<const NUM_THREADS: usize, const NUM_WORDS: usize>(
    kcas_state: &KCasState<NUM_THREADS, NUM_WORDS>,
    thread_id: ThreadId,
    kcas_words: [KCasWord; NUM_WORDS]
) -> Result<(), KCasError> {
    // thread ids are 1-indexed. Otherwise, we would be unable to tell the difference between thread 0 and an element with no thread id attached.
    let thread_index: usize = thread_id - 1;

    // first, get this thread's state ready
    // there is no need for CAS anywhere here because it is not possible for other threads to be
    // helping this thread yet
    let mut kcas_words = kcas_words;
    for row_num in 0..kcas_words.len() {
        let cas_row: &mut KCasWord = &mut kcas_words[row_num];

        let target_address: *mut AtomicUsize = unsafe { cas_row.target_address as *mut usize as *mut AtomicUsize };
        kcas_state.target_addresses[thread_index][row_num].store(target_address, Ordering::Release);

        kcas_state.expected_elements[thread_index][row_num].store(cas_row.expected_element, Ordering::Release);
        kcas_state.desired_elements[thread_index][row_num].store(cas_row.desired_element, Ordering::Release);
    }
    let original_stage_and_sequence: StageAndSequence = kcas_state.stage_and_sequence_numbers[thread_index].load(Ordering::Acquire);
    let sequence: SequenceNum = extract_sequence_from_stage_and_sequence(original_stage_and_sequence);

    // now we are ready to start "acquiring" slots
    let acquiring_stage_and_sequence: StageAndSequence = combine_stage_and_sequence(Stage::Claiming, sequence);
    kcas_state.stage_and_sequence_numbers[thread_index].store(acquiring_stage_and_sequence, Ordering::Release);

    match acquire(kcas_state, thread_id, sequence, 0usize) {
        Ok(_) => {
            // we've successfully acquired. Now we need to change our stage.
            match transition_stage_from_acquiring_to_setting(kcas_state, thread_id, sequence, acquiring_stage_and_sequence) {
                ContinueOrReturnEarly::Continue => {}
                ContinueOrReturnEarly::ReturnEarly(result) => { return result; }
            }
            set_and_reset(kcas_state, thread_id, sequence, 0).map_err(|fatal_error| FatalInternalError(fatal_error))
        },
        Err(acquire_error) => {
            match acquire_error {
                ClaimError::ConcurrentChangeError(concurrent_change_error) => {
                    // depends on what the concurrent change was
                    match concurrent_change_error {
                        ConcurrentChangeError::StageChanged { current_word_num, current_stage } => {
                            match current_stage {
                                Stage::Inactive => {
                                    // only this thread should have the ability to transition the stage to inactive
                                    Err(FatalError::IllegalStageTransition { original_stage: Stage::Setting, next_stage: Stage::Inactive }.into())
                                }
                                Stage::Claiming => {
                                    // this should also not be possible because our CAS expected the value to be Stage::Acquiring
                                    // and we were using strong CAS semantics
                                    return Err(FatalError::IllegalStageTransition { original_stage: Stage::Claiming, next_stage: Stage::Claiming }.into());
                                }
                                Stage::Setting => {
                                    // some other thread beat us to the setting stage. We can feel free to start setting now.
                                    set_and_reset(kcas_state, thread_id, sequence, 0)?;
                                    Ok(())
                                }
                                Stage::Reverting => {
                                    // some thread beat us to the chase and already started reverting, so we should revert, as well
                                    revert_and_reset(kcas_state, thread_id, sequence, NUM_WORDS)?;
                                    Err(KCasError::ValueWasNotExpectedValue)
                                }
                                Stage::Successful => {
                                    // some thread beat us to the chase and already completed the operation
                                    reset_for_next_operation(kcas_state, thread_id, sequence);
                                    Ok(())
                                }
                                Stage::Reverted => {
                                    // some thread beat us to the chase and already reverted the operation
                                    reset_for_next_operation(kcas_state, thread_id, sequence);
                                    Err(KCasError::ValueWasNotExpectedValue)
                                }
                            }
                        }
                        ConcurrentChangeError::SequenceChanged { .. } => {
                            Err(FatalError::ThreadSequenceModifiedByAnotherThread { thread_id }.into())
                        }
                        ConcurrentChangeError::StageBecameInvalid(out_of_bounds_stage_error) => {
                            Err(FatalError::OutOfBoundsStage(out_of_bounds_stage_error.0).into())
                        }
                    }

                }
                ClaimError::ValueWasDifferentThread { current_word_num, other_thread_id, other_sequence_num } => {
                    // help thread
                    Err(KCasError::HadToHelpThread)
                }
                ClaimError::ValueWasNotExpectedValue { current_word_num, actual_value } => {
                    // transition state to reverting
                    match transition_stage_from_acquiring_to_reverting(kcas_state, thread_id, sequence, acquiring_stage_and_sequence) {
                        ContinueOrReturnEarly::Continue => {}
                        ContinueOrReturnEarly::ReturnEarly(result) => { return result; }
                    }
                    revert_and_reset(kcas_state, thread_id, sequence, current_word_num)?;
                    Err(KCasError::ValueWasNotExpectedValue)
                }
                ClaimError::InvalidPointer => {
                    Err(FatalInternalError(FatalError::InvalidPointer))
                }
            }
        }
    }
}

fn transition_stage_from_acquiring_to_reverting<const NUM_THREADS: usize, const NUM_WORDS: usize>(
    kcas_state: &KCasState<NUM_THREADS, NUM_WORDS>,
    thread_id: ThreadId,
    sequence: SequenceNum,
    acquiring_stage_and_sequence: StageAndSequence,
) -> ContinueOrReturnEarly {
    let thread_index = thread_id - 1;

    let reverting_stage_and_sequence: StageAndSequence = combine_stage_and_sequence(Stage::Reverting, sequence);
    match kcas_state.stage_and_sequence_numbers[thread_index].compare_exchange(acquiring_stage_and_sequence, reverting_stage_and_sequence, Ordering::AcqRel, Ordering::Acquire) {
        Ok(_) => {
            ContinueOrReturnEarly::Continue
        }
        Err(actual_stage_and_sequence) => {
            // the compare and swap failed...
            // if the sequence changed, then something is terribly wrong, because we are the only one supposed to be able to change our sequence
            let actual_stage: Stage = match extract_stage_from_stage_and_sequence(actual_stage_and_sequence) {
                Ok(stage) => stage,
                Err(out_of_bounds_stage_error) => {
                    return ContinueOrReturnEarly::ReturnEarly(Err(FatalInternalError(FatalError::OutOfBoundsStage(out_of_bounds_stage_error.0))));
                }
            };
            let actual_sequence: SequenceNum = extract_sequence_from_stage_and_sequence(actual_stage_and_sequence);
            if actual_sequence != sequence {
                return ContinueOrReturnEarly::ReturnEarly(Err(FatalError::ThreadSequenceModifiedByAnotherThread { thread_id }.into()));
            }
            match actual_stage {
                Stage::Inactive => {
                    ContinueOrReturnEarly::ReturnEarly(Err(FatalInternalError(FatalError::IllegalStageTransition { original_stage: Stage::Claiming, next_stage: Stage::Inactive })))
                }
                Stage::Claiming => {
                    ContinueOrReturnEarly::ReturnEarly(Err(FatalInternalError(FatalError::IllegalStageTransition { original_stage: Stage::Claiming, next_stage: Stage::Claiming })))
                }
                Stage::Setting => {
                    // some other helper thread's acquire was successful and they modified the stage first
                    // they are already setting, so let's set as well
                    ContinueOrReturnEarly::ReturnEarly(set_and_reset(kcas_state, thread_id, sequence, 0).map_err(|fatal_err| FatalInternalError(fatal_err)))
                }
                Stage::Reverting => {
                    // we were already trying to make the stage Reverting, so this means
                    // another thread beat us to the chase, which is OK
                    ContinueOrReturnEarly::Continue
                }
                Stage::Successful => {
                    // some other thread's acquisition succeeded. They transitioned from acquiring
                    // to setting and then to Successful. We need to make sure to release any
                    // slots we acquired after the slots already transitioned to Successful
                    ContinueOrReturnEarly::ReturnEarly(revert_and_reset(kcas_state, thread_id, sequence, NUM_WORDS).map_err(|fatal_err| FatalInternalError(fatal_err)))
                }
                Stage::Reverted => {
                    // some other thread may have reverted entirely before we finished acquiring
                    // now we need to revert, as well
                    match revert_and_reset(kcas_state, thread_id, sequence, NUM_WORDS).map_err(|fatal_err| FatalInternalError(fatal_err)) {
                        Ok(_) => {
                            ContinueOrReturnEarly::ReturnEarly(Err(KCasError::ValueWasNotExpectedValue))
                        }
                        Err(kcas_error) => {
                            ContinueOrReturnEarly::ReturnEarly(Err(kcas_error))
                        }
                    }
                }
            }
        }
    }
}

enum ContinueOrReturnEarly {
    Continue,
    ReturnEarly(Result<(), KCasError>)
}

fn transition_stage_from_acquiring_to_setting<const NUM_THREADS: usize, const NUM_WORDS: usize>(
    kcas_state: &KCasState<NUM_THREADS, NUM_WORDS>,
    thread_id: ThreadId,
    sequence: SequenceNum,
    acquiring_stage_and_sequence: StageAndSequence,
) -> ContinueOrReturnEarly {
    let thread_index = thread_id - 1;

    let setting_stage_and_sequence: StageAndSequence = combine_stage_and_sequence(Stage::Setting, sequence);
    match kcas_state.stage_and_sequence_numbers[thread_index].compare_exchange(acquiring_stage_and_sequence, setting_stage_and_sequence, Ordering::AcqRel, Ordering::Acquire) {
        Ok(_) => {
            // if we were able to change our stage, then continue on to the setting stage
            ContinueOrReturnEarly::Continue
        }
        Err(actual_stage_and_sequence) => {
            // the compare and swap failed...
            // if the sequence changed, then something is terribly wrong, because we are the only one supposed to be able to change our sequence
            let actual_stage: Stage = match extract_stage_from_stage_and_sequence(actual_stage_and_sequence) {
                Ok(actual_stage) => actual_stage,
                Err(out_of_bounds_stage_error) => {
                    return ContinueOrReturnEarly::ReturnEarly(Err(FatalInternalError(FatalError::OutOfBoundsStage(out_of_bounds_stage_error.0))));
                }
            };
            let actual_sequence: SequenceNum = extract_sequence_from_stage_and_sequence(actual_stage_and_sequence);
            if actual_sequence != sequence {
                return ContinueOrReturnEarly::ReturnEarly(Err(FatalError::ThreadSequenceModifiedByAnotherThread { thread_id }.into()));
            }
            match actual_stage {
                // if the stage became successful, then another thread helped us and we are
                // ok to call this operation completed
                // but we do need to make sure we give up any slots we might have accidentally acquired
                // while the stage was already set to successful
                Stage::Successful => {
                    ContinueOrReturnEarly::ReturnEarly(
                        revert_and_reset(kcas_state, thread_id, sequence, NUM_WORDS)
                            .map_err(|fatal_error| FatalInternalError(fatal_error))
                    )
                }
                // if the stage became reverting, then it's possible that, while our acquire was seemingly successful,
                // another thread helping us had actually failed first and already started reverting.
                // because that helper thread may have already reverted some slots, we are forced to start reverting, as well
                Stage::Reverting => {
                    match revert_and_reset(kcas_state, thread_id, sequence, NUM_WORDS) {
                        Ok(_) => ContinueOrReturnEarly::ReturnEarly(Err(KCasError::ValueWasNotExpectedValue)),
                        Err(fatal_error) => ContinueOrReturnEarly::ReturnEarly(Err(FatalInternalError(fatal_error))),
                    }
                }
                Stage::Reverted => {
                    // some other thread helped us while we were acquiring, but their acquire failed
                    // so we need to revert, as well
                    match revert_and_reset(kcas_state, thread_id, sequence, NUM_WORDS) {
                        Ok(_) => ContinueOrReturnEarly::ReturnEarly(Err(KCasError::ValueWasNotExpectedValue)),
                        Err(fatal_error) => ContinueOrReturnEarly::ReturnEarly(Err(FatalInternalError(fatal_error))),
                    }
                }
                Stage::Inactive => {
                    // this should not be possible. Only we should be able to change the stage to inactive.
                    ContinueOrReturnEarly::ReturnEarly(Err(FatalInternalError(FatalError::IllegalStageTransition { original_stage: Stage::Claiming, next_stage: Stage::Inactive })))
                }
                Stage::Claiming => {
                    // this should also not be possible because our CAS expected the value to be Stage::Acquiring
                    // and we were using strong CAS semantics
                    ContinueOrReturnEarly::ReturnEarly(Err(FatalInternalError(FatalError::IllegalStageTransition { original_stage: Stage::Claiming, next_stage: Stage::Claiming })))
                }
                Stage::Setting => {
                    // this is OK because we were trying to set the stage to Setting, as well.
                    // this means some other thread beat us to the chase.
                    ContinueOrReturnEarly::Continue
                }
            }
        }
    }
}

fn set_and_reset<const NUM_THREADS: usize, const NUM_WORDS: usize>(
    kcas_state: &KCasState<NUM_THREADS, NUM_WORDS>,
    thread_id: ThreadId,
    sequence: SequenceNum,
    starting_word_num: usize,
) -> Result<(), FatalError> {
    match set(kcas_state, thread_id, sequence, starting_word_num) {
        Ok(_) => {
            // we've successfully set. This is the happiest path.
            reset_for_next_operation(kcas_state, thread_id, sequence);
            Ok(())
        },
        Err(set_error) => {
            match set_error {
                SetError::ConcurrentChangeError(concurrent_change_error) => {
                    match concurrent_change_error {
                        ConcurrentChangeError::StageChanged { current_stage, .. } => {
                            match current_stage {
                                // some other thread beat us while we were setting values
                                Stage::Successful => {
                                    reset_for_next_operation(kcas_state, thread_id, sequence);
                                    Ok(())
                                }
                                illegal_stage => {
                                    Err(FatalError::IllegalStageTransition { original_stage: Stage::Setting, next_stage: illegal_stage })
                                }
                            }
                        }
                        ConcurrentChangeError::SequenceChanged { .. } => {
                            Err(FatalError::ThreadSequenceModifiedByAnotherThread { thread_id })
                        }
                        ConcurrentChangeError::StageBecameInvalid(StageOutOfBoundsError(invalid_stage)) => {
                            Err(FatalError::OutOfBoundsStage(invalid_stage))
                        }
                    }
                }
                SetError::IllegalState => {
                    Err(FatalError::IllegalState)
                }
                SetError::InvalidPointer => {
                    Err(FatalError::InvalidPointer)
                }
            }
        }
    }
}

fn revert_and_reset<const NUM_THREADS: usize, const NUM_WORDS: usize>(
    kcas_state: &KCasState<NUM_THREADS, NUM_WORDS>,
    thread_id: ThreadId,
    sequence: SequenceNum,
    starting_word_num: usize,
) -> Result<(), FatalError> {
    let thread_index = thread_id - 1;

    match revert(kcas_state, thread_id, sequence, starting_word_num) {
        Ok(_) => {
            // revert succeeded, so we call this KCAS operation failed
            reset_for_next_operation(kcas_state, thread_id, sequence);
            Ok(())
        }
        Err(revert_error) => {
            match revert_error {
                RevertError::ConcurrentChangeError(concurrent_change_error) => {
                    match concurrent_change_error {
                        ConcurrentChangeError::StageChanged { current_word_num, current_stage } => {
                            // stage changed while reverting...
                            match current_stage {
                                Stage::Reverted => {
                                    // some other thread helped us revert
                                    // we call this operation failed
                                    reset_for_next_operation(kcas_state, thread_id, sequence);
                                    Ok(())
                                }
                                // there is no other state we could have moved to
                                invalid_stage => {
                                    Err(FatalError::IllegalStageTransition { original_stage: Stage::Reverting, next_stage: invalid_stage })
                                }
                            }
                        }
                        ConcurrentChangeError::SequenceChanged { current_word_num, current_sequence } => {
                            // if the sequence changed, then something is terribly wrong, because we are the only one supposed to be able to change our sequence
                            Err(FatalError::ThreadSequenceModifiedByAnotherThread { thread_id })
                        }
                        ConcurrentChangeError::StageBecameInvalid(out_of_bounds_stage_error) => {
                            Err(FatalError::OutOfBoundsStage(out_of_bounds_stage_error.0))
                        }
                    }
                }
                RevertError::InvalidPointer => {
                    Err(FatalError::InvalidPointer)
                }
            }
        }
    }
}

fn reset_for_next_operation<const NUM_THREADS: usize, const NUM_WORDS: usize>(
    kcas_state: &KCasState<NUM_THREADS, NUM_WORDS>,
    thread_id: ThreadId,
    sequence: SequenceNum,
) {
    let thread_index: usize = thread_id - 1;
    let next_sequence: SequenceNum = sequence + 1;
    let next_stage_and_sequence: StageAndSequence = combine_stage_and_sequence(Stage::Inactive, next_sequence);
    kcas_state.stage_and_sequence_numbers[thread_index].store(next_stage_and_sequence, Ordering::Release);
}

fn acquire<const NUM_THREADS: usize, const NUM_WORDS: usize>(
    kcas_state: &KCasState<NUM_THREADS, NUM_WORDS>,
    thread_id: ThreadId,
    sequence: SequenceNum,
    starting_word_num: usize,
) -> Result<(), ClaimError> {
    let thread_index: usize = thread_id - 1;
    let thread_and_sequence: ThreadAndSequence = combine_thread_id_and_sequence(thread_id, sequence, kcas_state.num_threads_bit_length);

    let mut current_word_num: usize = starting_word_num;
    while current_word_num < NUM_WORDS {
        let target_address: &AtomicPtr<AtomicUsize> = &kcas_state.target_addresses[thread_index][current_word_num];
        let target_address: *mut AtomicUsize = target_address.load(Ordering::Acquire);
        let target_reference: &AtomicUsize = unsafe { target_address.as_ref().ok_or_else(|| ClaimError::InvalidPointer)? };

        let expected_element: usize = kcas_state.expected_elements[thread_index][current_word_num].load(Ordering::Acquire);

        match target_reference.compare_exchange(expected_element, thread_and_sequence, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => {
                // we can consider the CAS operation succeeded only if the sequence has not changed and the stage has not changed
                verify_stage_and_sequence_have_not_changed(kcas_state, current_word_num, thread_index, Stage::Claiming, sequence)?;
                current_word_num += 1;
                continue;
            },
            Err(actual_value) => {
                verify_stage_and_sequence_have_not_changed(kcas_state, current_word_num, thread_index, Stage::Claiming, sequence)?;

                // the failed element was our own thread-and-sequence number, which means another thread helped us
                if actual_value == thread_and_sequence {
                    current_word_num += 1;
                    continue;
                }

                // the failed element was some other thread's thread-and-sequence number, which means we need to help another thread
                let possibly_other_thread_id: ThreadId = extract_thread_from_thread_and_sequence(thread_and_sequence, kcas_state.num_threads_bit_length);
                if possibly_other_thread_id != 0 {
                    let other_sequence_num = extract_sequence_from_thread_and_sequence(thread_and_sequence, kcas_state.sequence_mask_for_thread_and_sequence);
                    return Err(ClaimError::ValueWasDifferentThread { current_word_num, other_thread_id: possibly_other_thread_id, other_sequence_num, });
                }
                return Err(ClaimError::ValueWasNotExpectedValue { current_word_num, actual_value });
            }
        }
    }
    Ok(())
}

fn set<const NUM_THREADS: usize, const NUM_WORDS: usize>(
    kcas_state: &KCasState<NUM_THREADS, NUM_WORDS>,
    thread_id: ThreadId,
    sequence: SequenceNum,
    starting_word_num: usize,
) -> Result<(), SetError> {
    let thread_index: usize = thread_id - 1;
    let thread_and_sequence: ThreadAndSequence = combine_thread_id_and_sequence(thread_id, sequence, kcas_state.num_threads_bit_length);

    let mut current_word_num: usize = starting_word_num;
    while current_word_num < NUM_WORDS {
        let target_address: &AtomicPtr<AtomicUsize> = &kcas_state.target_addresses[thread_index][current_word_num];
        let target_address: *mut AtomicUsize = target_address.load(Ordering::Acquire);
        let target_reference: &AtomicUsize = unsafe { target_address.as_ref().ok_or_else(|| SetError::InvalidPointer)? };

        let desired_element: usize = kcas_state.desired_elements[thread_index][current_word_num].load(Ordering::Acquire);

        match target_reference.compare_exchange(thread_and_sequence, desired_element, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => {
                verify_stage_and_sequence_have_not_changed(kcas_state, current_word_num, thread_index, Stage::Setting, sequence)?;

                // the CAS operation succeeded, which means we should move on to the next element to CAS
                current_word_num += 1;
                continue;
            },
            Err(actual_value) => {
                verify_stage_and_sequence_have_not_changed(kcas_state, current_word_num, thread_index, Stage::Claiming, sequence)?;
                // it is possible, but not guaranteed, that another thread helped us.
                // in order for it to be the case that another thread helped us, the sequence number should match and the state should still be setting, completed, reverting, or reverted
                // but in our case, we should no longer continue if the state is anything other than setting
                if actual_value == desired_element {
                    current_word_num += 1;
                    continue;
                }
                return Err(SetError::IllegalState);
            }
        }
    }
    Ok(())
}

fn revert<const NUM_THREADS: usize, const NUM_WORDS: usize>(
    kcas_state: &KCasState<NUM_THREADS, NUM_WORDS>,
    thread_id: ThreadId,
    sequence: SequenceNum,
    starting_word_num: usize,
) -> Result<(), RevertError> {
    let thread_index: usize = thread_id - 1;
    let thread_and_sequence: ThreadAndSequence = combine_thread_id_and_sequence(thread_id, sequence, kcas_state.num_threads_bit_length);

    let mut current_word_num: usize = starting_word_num;
    while current_word_num > 0 {
        let target_address: &AtomicPtr<AtomicUsize> = &kcas_state.target_addresses[thread_index][current_word_num];
        let target_address: *mut AtomicUsize = target_address.load(Ordering::Acquire);
        let target_reference: &AtomicUsize = unsafe { target_address.as_ref().ok_or_else(|| RevertError::InvalidPointer)? };

        let expected_element: usize = kcas_state.expected_elements[thread_index][current_word_num].load(Ordering::Acquire);

        match target_reference.compare_exchange(thread_and_sequence, expected_element, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => {
                // the CAS operation succeeded, which means we should move on to the next element to CAS
                current_word_num -= 1;
                continue;
            },
            Err(actual_value) => {
                verify_stage_and_sequence_have_not_changed(kcas_state, current_word_num, thread_index, Stage::Reverting, sequence)?;
                // it is possible, but not guaranteed, that another thread helped us.
                // in order for it to be the case that another thread helped us, the sequence number should match and the state should still be setting, completed, reverting, or reverted
                // but in our case, we should no longer continue if the state is anything other than setting
                current_word_num -= 1;
                continue;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
pub mod tests {
    use crate::{kcas, KCasState, KCasWord};
    use crate::err::KCasError;
    use std::sync::mpsc::channel;

    #[test]
    fn test() {
        let kcas_state: KCasState<1, 3> = KCasState::new();
        // let (sender, receiver) = channel::<i32>();
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

        assert!(kcas(&kcas_state, 1, kcas_words).is_ok());

        println!("After KCAS first_location: {first_location}");
        println!("After KCAS second_location: {second_location}");
        println!("After KCAS third_location: {third_location}");
    }
}