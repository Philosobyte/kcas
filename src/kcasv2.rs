use core::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};
use crate::aliases::{SequenceNum, StageAndSequence, ThreadAndSequence, ThreadId, WordNum};
use crate::err::{ConcurrentChangeError, KCasError, StageOutOfBoundsError};
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

enum ChangeStageFromClaimingToSettingResult {

}

enum ChangeStageFromClaimingToRevertingResult {

}

enum ClaimResult {
    Claimed { claimed_word: WordNum },
    StageAndSequenceVerificationError { current_word_num: WordNum, error: StageAndSequenceVerificationError },
    ActualValueDidNotMatchExpectedValue { word_num: WordNum, actual_value: usize },
    ValueWasAnotherThreadAndSequence { word_num: WordNum, other_thread_and_sequence: ThreadAndSequence },
}

enum SetResult {

}

enum RevertResult {

}

enum HelpClaimResult {

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

}

enum StepResult {
    InitializeSharedState(SequenceNum),

    Claim(ClaimResult),
    ChangeStageFromClaimingToSetting(ChangeStageFromClaimingToSettingResult),
    ChangeStageFromClaimingToReverting(ChangeStageFromClaimingToRevertingResult),
    Set(SetResult),
    Revert(RevertResult),

    HelpClaim,
    HelpChangeStageFromClaimingToSetting,
    HelpChangeStageFromClaimingToReverting,
    HelpSet,
    HelpRevert,

    ClearState(Result<(), ClearStateResult>),
}

fn kcasv2<const NUM_THREADS: usize, const NUM_WORDS: usize>(
    shared_state: &SharedState<NUM_THREADS, NUM_WORDS>,
    thread_id: ThreadId,
    kcas_words: [KCasWord; NUM_WORDS]
) -> Result<(), KCasError> {
    let mut step_result: StepResult = initialize_shared_state(shared_state, thread_id, kcas_words);

    loop {
    }
}


fn perform_next_step<const NUM_THREADS: usize, const NUM_WORDS: usize>(
    kcas_state: &SharedState<NUM_THREADS, NUM_WORDS>,
    thread_id: ThreadId,
    step_result: StepResult,
) -> StepResult {
    match step_result {
        StepResult::InitializeSharedState(sequence) => {
            StepResult::Claim(claim(kcas_state, thread_id, sequence, 0))
        }
        StepResult::Claim(claim_result) => {
            match claim_result {
                ClaimResult::Claimed { claimed_word } => {}
                ClaimResult::StageAndSequenceVerificationError {  } => {}
                ClaimResult::ActualValueDidNotMatchExpectedValue { .. } => {}
                ClaimResult::ValueWasAnotherThreadAndSequence { .. } => {}
            }
        }
        StepResult::ChangeStageFromClaimingToSetting(_) => {}
        StepResult::ChangeStageFromClaimingToReverting(_) => {}
        StepResult::Set(_) => {}
        StepResult::Revert(_) => {}
        StepResult::HelpClaim => {}
        StepResult::HelpChangeStageFromClaimingToSetting => {}
        StepResult::HelpChangeStageFromClaimingToReverting => {}
        StepResult::HelpSet => {}
        StepResult::HelpRevert => {}
        StepResult::ClearState(_) => {}
    }
}

fn claim<const NUM_THREADS: usize, const NUM_WORDS: usize>(
    kcas_state: &SharedState<NUM_THREADS, NUM_WORDS>,
    thread_id: ThreadId,
    sequence: SequenceNum,
    starting_word_num: usize,
) -> ClaimResult {
    todo!()
}

fn initialize_shared_state<const NUM_THREADS: usize, const NUM_WORDS: usize>(
    shared_state: &SharedState<NUM_THREADS, NUM_WORDS>,
    thread_id: ThreadId,
    kcas_words: [KCasWord; NUM_WORDS]
) -> StepResult {
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

    StepResult::InitializeSharedState(sequence)
}