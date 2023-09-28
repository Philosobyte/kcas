#![cfg_attr(not(test), no_std)]

use core::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};
use crate::err::{AcquireError, ConcurrentChangeError, InvalidStageError, KCasError, RevertError, SetError};
use crate::stage::{Stage, STATUS_BIT_LENGTH};
use crate::types::{SequenceNumber, StageAndSequence, ThreadAndSequence, ThreadId};

mod err;
mod stage;
mod types;

/// The components for a single CAS operation.
#[derive(Debug, Eq, PartialEq)]
pub struct KCasWord<'a> {
    target_address: &'a mut usize,
    expected_element: usize,
    desired_element: usize,
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
fn combine_stage_and_sequence(stage: Stage, sequence: SequenceNumber) -> StageAndSequence {
    (stage as usize) << (usize::BITS as usize - STATUS_BIT_LENGTH) | sequence
}

/// Extract the Stage out of a number which holds both a stage and a sequence number.
/// The `Stage` is obtained from the [STATUS_BIT_LENGTH] most significant bits.
fn extract_stage_from_stage_and_sequence(
    stage_and_sequence: StageAndSequence
) -> Result<Stage, InvalidStageError> {
    let stage_as_num: usize = stage_and_sequence >> (usize::BITS as usize - STATUS_BIT_LENGTH);
    Stage::try_from(stage_as_num)
}

/// Extract the sequence number out of a number which holds both a stage and a sequence number.
/// The sequence number is obtained from the rest of the bits besides the [STATUS_BIT_LENGTH] most significant.
fn extract_sequence_from_stage_and_sequence(
    stage_and_sequence: StageAndSequence
) -> SequenceNumber {
    stage_and_sequence & SEQUENCE_MASK_FOR_STATUS_AND_SEQUENCE
}

fn combine_thread_id_and_sequence(
    thread_id: ThreadId,
    sequence: SequenceNumber,
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
) -> SequenceNumber {
    thread_and_sequence & sequence_mask_for_thread_and_sequence
}

fn verify_stage_and_sequence_have_not_changed<const NUM_THREADS: usize, const NUM_WORDS: usize>(
    thread_state: &KCasState<NUM_THREADS, NUM_WORDS>,
    current_word_num: usize,
    thread_index: usize,
    expected_stage: Stage,
    expected_sequence: SequenceNumber,
) -> Result<(), ConcurrentChangeError> {
    let current_stage_and_sequence: StageAndSequence = thread_state.stage_and_sequence_numbers[thread_index].load(Ordering::Acquire);
    let current_sequence: SequenceNumber = extract_sequence_from_stage_and_sequence(current_stage_and_sequence);
    let current_stage: Stage = extract_stage_from_stage_and_sequence(current_stage_and_sequence)?;

    if current_sequence != expected_sequence {
        return Err(ConcurrentChangeError::SequenceChanged { current_word_num, current_sequence });
    }
    if current_stage != expected_stage {
        return Err(ConcurrentChangeError::StageChanged { current_word_num, current_stage });
    }
    Ok(())
}

// fn verify_value_is_not_thread_and_sequence<const NUM_THREADS: usize, const NUM_WORDS: usize>(
// ) -> Result<(), KCasError> {
//     value:
// }

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
    for row_num in 0..kcas_words.len() {
        let cas_row: &KCasWord = &kcas_words[row_num];

        let target_address: *mut AtomicUsize = unsafe { cas_row.target_address as *mut usize as *mut AtomicUsize };
        kcas_state.target_addresses[thread_index][row_num].store(target_address, Ordering::Release);

        kcas_state.expected_elements[thread_index][row_num].store(cas_row.expected_element, Ordering::Release);
        kcas_state.desired_elements[thread_index][row_num].store(cas_row.desired_element, Ordering::Release);
    }
    let original_stage_and_sequence: StageAndSequence = kcas_state.stage_and_sequence_numbers[thread_index].load(Ordering::Acquire);
    let sequence: SequenceNumber = extract_sequence_from_stage_and_sequence(original_stage_and_sequence);

    // now we are ready to start "acquiring" slots
    let stage_and_sequence: StageAndSequence = combine_stage_and_sequence(Stage::Acquiring, sequence);
    kcas_state.stage_and_sequence_numbers[thread_index].store(stage_and_sequence, Ordering::Release);

    match acquire(kcas_state, thread_id, sequence, 0usize) {
        Ok(_) => {
            match set(kcas_state, thread_id, sequence, 0usize) {
                Ok(_) => {
                    let next_sequence: SequenceNumber = sequence + 1;
                    let next_stage_and_sequence: StageAndSequence = combine_stage_and_sequence(Stage::Inactive, next_sequence);
                    let result = kcas_state.stage_and_sequence_numbers[thread_index].compare_exchange(stage_and_sequence, next_stage_and_sequence, Ordering::AcqRel, Ordering::Acquire);
                    match result {
                        Ok(_) => {},
                        Err(actual_stage_and_sequence) => {
                            let actual_sequence: SequenceNumber = extract_sequence_from_stage_and_sequence(actual_stage_and_sequence);
                            let actual_stage: Stage = extract_stage_from_stage_and_sequence(actual_sequence)?;
                        }
                    }
                },
                Err(set_error) => {
                    match set_error {
                        SetError::ConcurrentChangeError(_) => {}
                        SetError::IllegalState => {}
                        SetError::InvalidPointer => {}
                    }
                }
            }
        },
        Err(acquire_error) => {
            match acquire_error {
                AcquireError::ConcurrentChangeError(_) => {}
                AcquireError::ValueWasDifferentThread { .. } => {}
                AcquireError::ValueWasAlreadyDesiredValue { .. } => {}
                AcquireError::ValueWasNotExpectedValue { .. } => {}
                AcquireError::InvalidPointer => {}
            }
        }
    }

    Ok(())
}

fn acquire<const NUM_THREADS: usize, const NUM_WORDS: usize>(
    kcas_state: &KCasState<NUM_THREADS, NUM_WORDS>,
    thread_id: ThreadId,
    sequence: SequenceNumber,
    starting_word_num: usize,
) -> Result<(), AcquireError> {
    let thread_index: usize = thread_id - 1;
    let thread_and_sequence: ThreadAndSequence = combine_thread_id_and_sequence(thread_id, sequence, kcas_state.num_threads_bit_length);

    let mut current_word_num: usize = starting_word_num;
    while current_word_num < NUM_WORDS {
        let target_address: &AtomicPtr<AtomicUsize> = &kcas_state.target_addresses[thread_index][current_word_num];
        let target_address: *mut AtomicUsize = target_address.load(Ordering::Acquire);
        let target_reference: &AtomicUsize = unsafe { target_address.as_ref().ok_or_else(|| AcquireError::InvalidPointer)? };

        let expected_element: usize = kcas_state.expected_elements[thread_index][current_word_num].load(Ordering::Acquire);

        match target_reference.compare_exchange(expected_element, thread_and_sequence, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => {
                // we can consider the CAS operation succeeded only if the sequence has not changed and the stage has not changed
                verify_stage_and_sequence_have_not_changed(kcas_state, current_word_num, thread_index, Stage::Acquiring, sequence)?;
                current_word_num += 1;
                continue;
            },
            Err(actual_value) => {
                verify_stage_and_sequence_have_not_changed(kcas_state, current_word_num, thread_index, Stage::Acquiring, sequence)?;

                // the failed element was our own thread-and-sequence number, which means another thread helped us
                if actual_value == thread_and_sequence {
                    current_word_num += 1;
                    continue;
                }

                // the failed element was some other thread's thread-and-sequence number, which means we need to help another thread
                let possibly_other_thread_id: ThreadId = extract_thread_from_thread_and_sequence(thread_and_sequence, kcas_state.num_threads_bit_length);
                if possibly_other_thread_id != 0 {
                    let other_sequence_num = extract_sequence_from_thread_and_sequence(thread_and_sequence, kcas_state.sequence_mask_for_thread_and_sequence);
                    return Err(AcquireError::ValueWasDifferentThread { current_word_num, other_thread_id: possibly_other_thread_id, other_sequence_num, });
                }

                if actual_value == expected_element {
                    return Err(AcquireError::ValueWasAlreadyDesiredValue { current_word_num });
                }
                return Err(AcquireError::ValueWasNotExpectedValue { current_word_num, actual_value });
            }
        }
    }
    Ok(())
}

fn set<const NUM_THREADS: usize, const NUM_WORDS: usize>(
    kcas_state: &KCasState<NUM_THREADS, NUM_WORDS>,
    thread_id: ThreadId,
    sequence: SequenceNumber,
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
                verify_stage_and_sequence_have_not_changed(kcas_state, current_word_num, thread_index, Stage::Acquiring, sequence)?;
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
    sequence: SequenceNumber,
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
