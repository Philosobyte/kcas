use alloc::sync::Arc;
use core::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};
use crate::aliases::{SequenceNum, StageAndSequence, thread_id_to_thread_index, thread_index_to_thread_id, ThreadAndSequence, ThreadId, ThreadIndex, WordNum};
use crate::err::StageOutOfBoundsError;
use crate::stage::{Stage, STATUS_BIT_LENGTH};

#[derive(Debug, Eq, PartialEq)]
pub struct KCasWord<'a> {
    target_address: &'a mut usize,
    expected_element: usize,
    desired_element: usize,
}

impl<'a> KCasWord<'a> {
    pub fn new(target_address: &'a mut usize, expected_element: usize, desired_element: usize) -> Self {
        Self { target_address, expected_element, desired_element }
    }
}

// pub struct KCasExecutor<const NUM_THREADS: usize, const NUM_WORDS: usize> {
//     shared_state: Arc<>
// }

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

// errors
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

pub enum KCasError {
    Fatal(FatalError),
    ValueWasNotExpectedValue,
}

impl From<FatalError> for KCasError {
    fn from(fatal_error: FatalError) -> Self {
        KCasError::Fatal(fatal_error)
    }
}

pub enum FatalError {
    StageOutOfBounds(usize),
    IllegalStageChange { original_stage: Stage, actual_stage: Stage },
    IllegalHelpeeStage(Stage),
    TargetAddressWasNotValidPointer(WordNum),
    SequenceNumChangedByNonOriginalThread { original_thread_id: ThreadId },
    TriedToSetValueWhichWasNotClaimMarker { word_num: WordNum, actual_value: usize },
}

impl From<StageOutOfBoundsError> for FatalError {
    fn from(stage_out_of_bounds_error: StageOutOfBoundsError) -> Self {
        FatalError::StageOutOfBounds(stage_out_of_bounds_error.0)
    }
}

pub fn kcas<const NUM_THREADS: usize, const NUM_WORDS: usize>(
    shared_state: &SharedState<NUM_THREADS, NUM_WORDS>,
    thread_id: ThreadId,
    kcas_words: [KCasWord; NUM_WORDS]
) -> Result<(), KCasError> {
    let thread_index: usize = thread_id - 1;

    let sequence: SequenceNum = initialize_operation(shared_state, thread_index, kcas_words);
    claim_and_transition(
        shared_state,
        thread_id_to_thread_index(thread_id),
        sequence,
        combine_thread_id_and_sequence(thread_id, sequence, shared_state.num_threads_bit_length)
    )
}

// state transition functions
fn claim_and_transition<const NUM_THREADS: usize, const NUM_WORDS: usize>(
    shared_state: &SharedState<NUM_THREADS, NUM_WORDS>,
    thread_index: ThreadIndex,
    sequence: SequenceNum,
    thread_and_sequence: ThreadAndSequence,
) -> Result<(), KCasError> {
    let claim_result: Result<(), ClaimError> = claim(shared_state, thread_index, sequence, thread_and_sequence);
    if claim_result.is_ok() {
        let stage_and_sequence: ThreadAndSequence = combine_stage_and_sequence(Stage::Claiming, sequence);
        return change_stage_and_transition(
            shared_state,
            thread_index,
            sequence,
            thread_and_sequence,
            stage_and_sequence,
            Stage::Claiming,
            Stage::Setting,
            || set_and_transition(shared_state, thread_index, sequence, thread_and_sequence)
        );
    }
    match claim_result.unwrap_err() {
        ClaimError::StageAndSequenceVerificationError { failed_word_num, error } => {
            handle_concurrent_stage_and_sequence_verification_error_and_transition(shared_state, thread_index, sequence, thread_and_sequence, Stage::Claiming, error)
        }
        ClaimError::ValueWasNotExpectedValue { word_num, actual_value } => {
            change_stage_and_transition(
                shared_state,
                thread_index,
                sequence,
                thread_and_sequence,
                combine_stage_and_sequence(Stage::Claiming, sequence),
                Stage::Claiming,
                Stage::Reverting,
                || revert_and_transition(
                    shared_state,
                    thread_index,
                    thread_and_sequence,
                    word_num,
                    || {
                        reset_for_next_operation(shared_state, thread_index, sequence);
                        Err(KCasError::ValueWasNotExpectedValue)
                    }
                )
            )
        }
        ClaimError::TargetAddressWasNotValidPointer(failed_word_num) => {
            Err(KCasError::Fatal(FatalError::TargetAddressWasNotValidPointer(failed_word_num)))
        }
        ClaimError::FatalErrorWhileHelping(fatal_error) => {
            Err(KCasError::Fatal(fatal_error))
        }
    }
}

fn set_and_transition<const NUM_THREADS: usize, const NUM_WORDS: usize>(
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

fn help_set_and_transition<const NUM_THREADS: usize, const NUM_WORDS: usize>(
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

fn revert_and_transition<const NUM_THREADS: usize, const NUM_WORDS: usize, F>(
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

fn help_revert_and_transition<const NUM_THREADS: usize, const NUM_WORDS: usize, F>(
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

// stage change functions
fn change_stage_and_transition<const NUM_THREADS: usize, const NUM_WORDS: usize, F>(
    shared_state: &SharedState<NUM_THREADS, NUM_WORDS>,
    thread_index: ThreadIndex,
    sequence: SequenceNum,
    thread_and_sequence: ThreadAndSequence,
    current_stage_and_sequence: StageAndSequence,
    current_stage: Stage,
    desired_stage: Stage,
    next_function: F,
) -> Result<(), KCasError>
    where F: FnOnce() -> Result<(), KCasError>
{
    let desired_stage_and_sequence: StageAndSequence = combine_stage_and_sequence(desired_stage, sequence);

    let cas_result: Result<usize, usize> = shared_state.stage_and_sequence_numbers[thread_index].compare_exchange(current_stage_and_sequence, desired_stage_and_sequence, Ordering::AcqRel, Ordering::Acquire);
    if cas_result.is_ok() {
        return next_function();
    }
    let actual_stage_and_sequence: StageAndSequence = cas_result.unwrap_err();
    let actual_stage: Stage = extract_stage_from_stage_and_sequence(actual_stage_and_sequence)
        .map_err(|e| FatalError::from(e))?;
    let actual_sequence: SequenceNum = extract_sequence_from_stage_and_sequence(actual_stage_and_sequence);

    if actual_sequence != sequence {
        return revert_and_transition(
            shared_state,
            thread_index,
            thread_and_sequence,
            NUM_WORDS - 1,
            || Err(KCasError::Fatal(FatalError::SequenceNumChangedByNonOriginalThread { original_thread_id: thread_index_to_thread_id(thread_index) }))
        );
    }
    handle_concurrent_stage_change_and_transition(shared_state, thread_index, sequence, thread_and_sequence, current_stage, actual_stage)
}

fn help_change_stage_and_transition<const NUM_THREADS: usize, const NUM_WORDS: usize, F>(
    shared_state: &SharedState<NUM_THREADS, NUM_WORDS>,
    thread_index: ThreadIndex,
    sequence: SequenceNum,
    thread_and_sequence: ThreadAndSequence,
    current_stage_and_sequence: StageAndSequence,
    current_stage: Stage,
    desired_stage: Stage,
    next_function: F,
) -> Result<(), HelpError>
    where F: FnOnce() -> Result<(), HelpError>
{
    let desired_stage_and_sequence: StageAndSequence = combine_stage_and_sequence(desired_stage, sequence);

    let cas_result: Result<usize, usize> = shared_state.stage_and_sequence_numbers[thread_index].compare_exchange(current_stage_and_sequence, desired_stage_and_sequence, Ordering::AcqRel, Ordering::Acquire);
    if cas_result.is_ok() {
        return next_function();
    }
    let actual_stage_and_sequence: StageAndSequence = cas_result.unwrap_err();
    let actual_stage: Stage = extract_stage_from_stage_and_sequence(actual_stage_and_sequence)
        .map_err(|e| FatalError::from(e))?;
    let actual_sequence: SequenceNum = extract_sequence_from_stage_and_sequence(actual_stage_and_sequence);

    if actual_sequence != sequence {
        return help_revert_and_transition(
            shared_state,
            thread_index,
            thread_and_sequence,
            NUM_WORDS - 1,
            || Err(HelpError::SequenceChangedWhileHelping)
        );
    }
    help_handle_concurrent_stage_change_and_transition(shared_state, thread_index, sequence, thread_and_sequence, current_stage, actual_stage)
}

fn handle_concurrent_stage_and_sequence_verification_error_and_transition<const NUM_THREADS: usize, const NUM_WORDS: usize>(
    shared_state: &SharedState<NUM_THREADS, NUM_WORDS>,
    thread_index: ThreadIndex,
    sequence: SequenceNum,
    thread_and_sequence: ThreadAndSequence,
    current_stage: Stage,
    error: StageAndSequenceVerificationError,
) -> Result<(), KCasError> {
    match error {
        StageAndSequenceVerificationError::StageChanged { actual_stage } => {
            handle_concurrent_stage_change_and_transition(shared_state, thread_index, sequence, thread_and_sequence, current_stage, actual_stage)
        }
        StageAndSequenceVerificationError::SequenceChanged { actual_sequence } => {
            Err(KCasError::Fatal(FatalError::SequenceNumChangedByNonOriginalThread { original_thread_id: thread_index_to_thread_id(thread_index) }))
        }
        StageAndSequenceVerificationError::StageOutOfBounds { actual_stage_number } => {
            Err(KCasError::Fatal(FatalError::StageOutOfBounds(actual_stage_number)))
        }
    }
}

fn help_handle_concurrent_stage_and_sequence_verification_error_and_transition<const NUM_THREADS: usize, const NUM_WORDS: usize>(
    shared_state: &SharedState<NUM_THREADS, NUM_WORDS>,
    thread_index: ThreadIndex,
    sequence: SequenceNum,
    thread_and_sequence: ThreadAndSequence,
    current_stage: Stage,
    error: StageAndSequenceVerificationError,
) -> Result<(), HelpError> {
    match error {
        StageAndSequenceVerificationError::StageChanged { actual_stage } => {
            help_handle_concurrent_stage_change_and_transition(shared_state, thread_index, sequence, thread_and_sequence, current_stage, actual_stage)
        }
        StageAndSequenceVerificationError::SequenceChanged { .. } => {
            Err(HelpError::SequenceChangedWhileHelping)
        }
        StageAndSequenceVerificationError::StageOutOfBounds { actual_stage_number } => {
            Err(HelpError::Fatal(FatalError::StageOutOfBounds(actual_stage_number)))
        }
    }
}

fn handle_concurrent_stage_change_and_transition<const NUM_THREADS: usize, const NUM_WORDS: usize>(
    shared_state: &SharedState<NUM_THREADS, NUM_WORDS>,
    thread_index: ThreadIndex,
    sequence: SequenceNum,
    thread_and_sequence: ThreadAndSequence,
    current_stage: Stage,
    actual_stage: Stage,
) -> Result<(), KCasError> {
    match actual_stage {
        // not possible - fatal - don't even bother reverting
        Stage::Inactive | Stage::Claiming => {
            Err(KCasError::Fatal(FatalError::IllegalStageChange { original_stage: current_stage, actual_stage }))
        }
        Stage::Setting => {
            set_and_transition(shared_state, thread_index, sequence, thread_and_sequence)
        }
        Stage::Reverting | Stage::Reverted => {
            revert_and_transition(
                shared_state,
                thread_index,
                thread_and_sequence,
                NUM_WORDS - 1,
                || {
                    reset_for_next_operation(shared_state, thread_index, sequence);
                    Err(KCasError::ValueWasNotExpectedValue)
                }
            )
        }
        Stage::Successful => {
            revert_and_transition(
                shared_state,
                thread_index,
                thread_and_sequence,
                NUM_WORDS - 1,
                || {
                    reset_for_next_operation(shared_state, thread_index, sequence);
                    Ok(())
                }
            )
        }
    }
}

fn help_handle_concurrent_stage_change_and_transition<const NUM_THREADS: usize, const NUM_WORDS: usize>(
    shared_state: &SharedState<NUM_THREADS, NUM_WORDS>,
    thread_index: ThreadIndex,
    sequence: SequenceNum,
    thread_and_sequence: ThreadAndSequence,
    current_stage: Stage,
    actual_stage: Stage,
) -> Result<(), HelpError> {
    match actual_stage {
        // not possible - fatal - don't even bother reverting
        Stage::Inactive | Stage::Claiming => {
            Err(HelpError::Fatal(FatalError::IllegalStageChange { original_stage: current_stage, actual_stage }))
        }
        Stage::Setting => {
            help_set_and_transition(shared_state, thread_index, sequence, thread_and_sequence)
        }
        Stage::Reverting => {
            help_revert_and_transition(
                shared_state,
                thread_index,
                thread_and_sequence,
                NUM_WORDS - 1,
                || {
                    help_change_stage_and_transition(
                        shared_state,
                        thread_index,
                        sequence,
                        thread_and_sequence,
                        combine_stage_and_sequence(Stage::Reverting, sequence),
                        Stage::Reverting,
                        Stage::Reverted,
                        || Ok(())
                    )
                }
            )
        }
        Stage::Reverted | Stage::Successful => {
            help_revert_and_transition(
                shared_state,
                thread_index,
                thread_and_sequence,
                NUM_WORDS - 1,
                || {
                    Ok(())
                }
            )
        }
    }
}


// initialization and teardown
fn initialize_operation<const NUM_THREADS: usize, const NUM_WORDS: usize>(
    shared_state: &SharedState<NUM_THREADS, NUM_WORDS>,
    thread_index: ThreadIndex,
    kcas_words: [KCasWord; NUM_WORDS]
) -> SequenceNum {
    // no need for CAS anywhere here because it is not possible for other threads to be helping yet
    let mut kcas_words = kcas_words;
    for row_num in 0..kcas_words.len() {
        let kcas_word: &mut KCasWord = &mut kcas_words[row_num];

        let target_address: *mut AtomicUsize = unsafe { kcas_word.target_address as *mut usize as *mut AtomicUsize };
        shared_state.target_addresses[thread_index][row_num].store(target_address, Ordering::Release);

        shared_state.expected_elements[thread_index][row_num].store(kcas_word.expected_element, Ordering::Release);
        shared_state.desired_elements[thread_index][row_num].store(kcas_word.desired_element, Ordering::Release);
    }
    let original_stage_and_sequence: StageAndSequence = shared_state.stage_and_sequence_numbers[thread_index].load(Ordering::Acquire);
    let sequence: SequenceNum = extract_sequence_from_stage_and_sequence(original_stage_and_sequence);

    // now we are ready to start "acquiring" slots
    let acquiring_stage_and_sequence: StageAndSequence = combine_stage_and_sequence(Stage::Claiming, sequence);
    shared_state.stage_and_sequence_numbers[thread_index].store(acquiring_stage_and_sequence, Ordering::Release);

    sequence
}

fn reset_for_next_operation<const NUM_THREADS: usize, const NUM_WORDS: usize>(
    thread_state: &SharedState<NUM_THREADS, NUM_WORDS>,
    thread_index: ThreadIndex,
    sequence: SequenceNum,
) {
    let next_sequence: SequenceNum = sequence + 1;
    let next_stage_and_sequence: StageAndSequence = combine_stage_and_sequence(Stage::Inactive, next_sequence);
    thread_state.stage_and_sequence_numbers[thread_index].store(next_stage_and_sequence, Ordering::Release);
}

// lowest level functions
enum ClaimError {
    StageAndSequenceVerificationError { failed_word_num: WordNum, error: StageAndSequenceVerificationError },
    ValueWasNotExpectedValue { word_num: WordNum, actual_value: usize },
    TargetAddressWasNotValidPointer(WordNum),
    FatalErrorWhileHelping(FatalError),
}

fn claim<const NUM_THREADS: usize, const NUM_WORDS: usize>(
    shared_state: &SharedState<NUM_THREADS, NUM_WORDS>,
    thread_index: ThreadIndex,
    sequence: SequenceNum,
    thread_and_sequence: ThreadAndSequence,
) -> Result<(), ClaimError> {
    let mut word_num: WordNum = 0;
    while word_num < NUM_WORDS {
        let target_address: &AtomicPtr<AtomicUsize> = &shared_state.target_addresses[thread_index][word_num];
        let target_address: *mut AtomicUsize = target_address.load(Ordering::Acquire);
        let target_reference: &AtomicUsize = unsafe {
            target_address.as_ref().ok_or_else(|| ClaimError::TargetAddressWasNotValidPointer(word_num))?
        };

        let expected_element: usize = shared_state.expected_elements[thread_index][word_num].load(Ordering::Acquire);

        match target_reference.compare_exchange(expected_element, thread_and_sequence, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => { word_num += 1; },
            Err(actual_value) => {
                if actual_value == thread_and_sequence {
                    word_num += 1;
                    continue;
                }

                if let Err(error) = verify_stage_and_sequence_have_not_changed(shared_state, thread_index, Stage::Claiming, sequence) {
                    return Err(ClaimError::StageAndSequenceVerificationError { failed_word_num: word_num, error });
                }

                match extract_thread_from_thread_and_sequence(actual_value, shared_state.num_threads_bit_length) {
                    0 => return Err(ClaimError::ValueWasNotExpectedValue { word_num, actual_value }),
                    other_thread_id => {
                        // help thread, then continue
                        match help_thread(shared_state, actual_value) {
                            Ok(_) => {
                                // try this iteration again
                            }
                            Err(help_error) => {
                                match help_error {
                                    HelpError::SequenceChangedWhileHelping | HelpError::HelpeeStageIsAlreadyTerminal(_) => {
                                        // try this iteration again
                                    }
                                    HelpError::Fatal(fatal_error) => {
                                        return Err(ClaimError::FatalErrorWhileHelping(fatal_error));
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

enum RevertError {
    TargetAddressWasNotValidPointer(WordNum),
}

fn revert<const NUM_THREADS: usize, const NUM_WORDS: usize>(
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

fn help_thread<const NUM_THREADS: usize, const NUM_WORDS: usize>(
    shared_state: &SharedState<NUM_THREADS, NUM_WORDS>,
    encountered_thread_and_sequence: ThreadAndSequence,
) -> Result<(), HelpError> {
    let thread_id: ThreadId = extract_thread_from_thread_and_sequence(encountered_thread_and_sequence, shared_state.num_threads_bit_length);
    let thread_index: ThreadIndex = thread_id_to_thread_index(thread_id);
    let encountered_sequence: SequenceNum = extract_sequence_from_thread_and_sequence(encountered_thread_and_sequence, shared_state.num_threads_bit_length);

    let stage_and_sequence: StageAndSequence = shared_state.stage_and_sequence_numbers[thread_index].load(Ordering::Acquire);
    let stage: Stage = extract_stage_from_stage_and_sequence(stage_and_sequence)
        .map_err(|stage_out_of_bounds_error| FatalError::StageOutOfBounds(stage_out_of_bounds_error.0))?;
    let sequence: SequenceNum = extract_sequence_from_stage_and_sequence(stage_and_sequence);

    if sequence != encountered_sequence {
        // the helpee's sequence is already different than when the helper encountered the helpee's
        // thread_and_sequence. This means the helpee's thread_and_sequence must no longer be there
        // and the helper can retry its own operation.
        return Err(HelpError::SequenceChangedWhileHelping);
    }
    match stage {
        Stage::Inactive => {
            Err(HelpError::Fatal(FatalError::IllegalHelpeeStage(Stage::Inactive)))
        }
        Stage::Claiming => {
            help_claim_and_transition(
                shared_state,
                thread_index,
                sequence,
                encountered_thread_and_sequence
            )
        }
        Stage::Setting => {
            help_set_and_transition(
                shared_state,
                thread_index,
                sequence,
                encountered_thread_and_sequence
            )
        }
        Stage::Reverting => {
            help_revert_and_transition(
                shared_state,
                thread_index,
                encountered_thread_and_sequence,
                NUM_WORDS - 1,
                || {
                    help_change_stage_and_transition(
                        shared_state,
                        thread_index,
                        sequence,
                        encountered_thread_and_sequence,
                        combine_stage_and_sequence(Stage::Reverting, sequence),
                        Stage::Reverting,
                        Stage::Reverted,
                        || Ok(())
                    )
                }
            )
        }
        Stage::Successful | Stage::Reverted => {
            Err(HelpError::HelpeeStageIsAlreadyTerminal(stage))
        }
    }
}

enum HelpError {
    SequenceChangedWhileHelping,
    HelpeeStageIsAlreadyTerminal(Stage),
    Fatal(FatalError)
}

impl From<FatalError> for HelpError {
    fn from(fatal_error: FatalError) -> Self {
        Self::Fatal(fatal_error)
    }
}

fn help_claim_and_transition<const NUM_THREADS: usize, const NUM_WORDS: usize>(
    shared_state: &SharedState<NUM_THREADS, NUM_WORDS>,
    thread_index: ThreadIndex,
    sequence: SequenceNum,
    thread_and_sequence: ThreadAndSequence,
) -> Result<(), HelpError> {
    let claim_result: Result<(), ClaimError> = claim(shared_state, thread_index, sequence, thread_and_sequence);
    if claim_result.is_ok() {
        let stage_and_sequence: ThreadAndSequence = combine_stage_and_sequence(Stage::Claiming, sequence);
        return help_change_stage_and_transition(
            shared_state,
            thread_index,
            sequence,
            thread_and_sequence,
            stage_and_sequence,
            Stage::Claiming,
            Stage::Setting,
            || help_set_and_transition(shared_state, thread_index, sequence, thread_and_sequence)
        );
    }
    match claim_result.unwrap_err() {
        ClaimError::StageAndSequenceVerificationError { failed_word_num, error } => {
            help_handle_concurrent_stage_and_sequence_verification_error_and_transition(shared_state, thread_index, sequence, thread_and_sequence, Stage::Claiming, error)
        }
        ClaimError::ValueWasNotExpectedValue { word_num, actual_value } => {
            help_change_stage_and_transition(
                shared_state,
                thread_index,
                sequence,
                thread_and_sequence,
                combine_stage_and_sequence(Stage::Claiming, sequence),
                Stage::Claiming,
                Stage::Reverting,
                || help_revert_and_transition(
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
                            || Ok(())
                        )
                    }
                )
            )
        }
        ClaimError::TargetAddressWasNotValidPointer(failed_word_num) => {
            Err(HelpError::Fatal(FatalError::TargetAddressWasNotValidPointer(failed_word_num)))
        }
        ClaimError::FatalErrorWhileHelping(fatal_error) => {
            Err(HelpError::Fatal(fatal_error))
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::kcasv3::{kcas, KCasWord, SharedState};

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

        assert!(kcas(&shared_state, 1, kcas_words).is_ok());

        println!("After KCAS first_location: {first_location}");
        println!("After KCAS second_location: {second_location}");
        println!("After KCAS third_location: {third_location}");
    }

    #[test]
    fn test_multiple_threads() {
        let shared_state: SharedState<2, 3> = SharedState::new();

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

        assert!(kcas(&shared_state, 1, kcas_words).is_ok());

        println!("After KCAS first_location: {first_location}");
        println!("After KCAS second_location: {second_location}");
        println!("After KCAS third_location: {third_location}");
    }
}
