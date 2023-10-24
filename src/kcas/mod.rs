use crate::err::{Error, FatalError};
use crate::kcas::claim::{claim_and_transition, help_claim_and_transition};
use crate::kcas::revert::help_revert_and_transition;
use crate::kcas::set::help_set_and_transition;
use crate::kcas::stage_change::help_change_stage_and_transition;
use crate::sync::{AtomicPtr, AtomicUsize, Ordering};
use crate::types::{
    construct_stage_and_sequence, construct_thread_and_sequence, convert_thread_id_to_thread_index,
    extract_sequence_from_stage_and_sequence, extract_sequence_from_thread_and_sequence,
    extract_stage_from_stage_and_sequence, extract_thread_from_thread_and_sequence,
    get_sequence_mask_for_thread_and_sequence, SequenceNum, Stage, StageAndSequence,
    ThreadAndSequence, ThreadId, ThreadIndex,
};
use core::sync::atomic::AtomicBool;
use displaydoc::Display;

use tracing::{instrument, trace};

mod claim;
mod revert;
mod set;
mod stage_change;

/// A structure containing all the information needed to perform a single CAS operation.
///
/// `target_address` is a reference whose value should be compared and swapped. Values
/// `expected_element` and `desired_element` can be any usize, including a thin pointer, as long as
/// the most significant bits are either all 0s or all 1s. The number of bits which must follow
/// this rule scales with `NUM_THREADS`. For example, on a 64-bit system, if we would like the
/// lower 52 bits to be free to hold arbitrary content, then the maximum allowed `NUM_THREADS` is
/// 4096, since that is the largest number which fits in `(64 - 52) = 12` bits.
///
/// These bits are used
/// internally by kcas to differentiate between temporary [ThreadAndSequence] markers and real
/// values.
///
/// The reason the most significant bits in values are allowed to be either all 0s or all 1s is to
/// allow sign extension, which is required by canonical pointers in some operating systems.
#[derive(Clone, Debug)]
pub struct KCasWord<'a> {
    target_address: &'a AtomicUsize,
    expected_value: usize,
    desired_value: usize,
}

impl<'a> KCasWord<'a> {
    pub fn new(
        target_address: &'a AtomicUsize,
        expected_element: usize,
        desired_element: usize,
    ) -> Self {
        Self {
            target_address,
            expected_value: expected_element,
            desired_value: desired_element,
        }
    }
}

/// Holds KCAS operation state shared between all threads. This allows threads to help each other.
///
/// [NUM_THREADS] is the maximum number of threads allowed to perform KCAS operations at
/// any given point in time. Memory is allocated at initialization for each thread.
///
/// [NUM_WORDS] is the number of [KCasWord]s to CAS during every operation. Memory is allocated at
/// initialization for each thread. It is currently expected that each KCAS operation involves
/// exactly [NUM_WORDS] words, although it may become possible to support operations from 0 to
/// [NUM_WORDS] in the future.
#[derive(Debug)]
pub struct State<const NUM_THREADS: usize, const NUM_WORDS: usize> {
    /// The number of wrappers (typically one per thread) which are currently assigned a [ThreadId]
    /// and able to perform KCAS operations.
    pub(crate) num_threads_in_use: AtomicUsize,

    /// Indicates whether a particular [ThreadId] is currently assigned to a wrapper.
    pub(crate) thread_index_slots: [AtomicBool; NUM_THREADS],

    /// Stores the target address pointer of each [KCasWord] for the current KCAS operation for
    /// each thread.
    pub(crate) target_addresses: [[AtomicPtr<AtomicUsize>; NUM_WORDS]; NUM_THREADS],

    /// Stores the expected value of each [KCasWord] for the current KCAS operation for each thread.
    pub(crate) expected_values: [[AtomicUsize; NUM_WORDS]; NUM_THREADS],

    /// Stores the desired value of each [KCasWord] for the current KCAS operation for each thread.
    pub(crate) desired_values: [[AtomicUsize; NUM_WORDS]; NUM_THREADS],

    /// The current [StageAndSequence] for each [ThreadId].
    pub(crate) stage_and_sequence_numbers: [AtomicUsize; NUM_THREADS],
}

impl<const NUM_THREADS: usize, const NUM_WORDS: usize> State<NUM_THREADS, NUM_WORDS> {
    pub fn new() -> Self {
        Self {
            num_threads_in_use: AtomicUsize::default(),
            thread_index_slots: core::array::from_fn(|_| AtomicBool::default()),
            target_addresses: core::array::from_fn(|_| {
                core::array::from_fn(|_| AtomicPtr::default())
            }),
            expected_values: core::array::from_fn(|_| {
                core::array::from_fn(|_| AtomicUsize::default())
            }),
            desired_values: core::array::from_fn(|_| {
                core::array::from_fn(|_| AtomicUsize::default())
            }),
            stage_and_sequence_numbers: core::array::from_fn(|_| AtomicUsize::default()),
        }
    }
}

/// Perform a single KCAS operation on `kcas_words`.
#[instrument]
pub(crate) fn kcas<const NUM_THREADS: usize, const NUM_WORDS: usize>(
    shared_state: &State<NUM_THREADS, NUM_WORDS>,
    thread_id: ThreadId,
    kcas_words: [KCasWord; NUM_WORDS],
) -> Result<(), Error> {
    let thread_index: ThreadIndex = convert_thread_id_to_thread_index(thread_id);
    let sequence: SequenceNum = initialize_operation(shared_state, thread_index, kcas_words);

    let thread_and_sequence: ThreadAndSequence =
        construct_thread_and_sequence::<NUM_THREADS>(thread_id, sequence);
    // kick off tree of fn calls
    let result = claim_and_transition(shared_state, thread_index, sequence, thread_and_sequence);
    trace!("result: {result:?}");
    result
}

/// Persist information about this operation at [ThreadIndex] which may be needed by other threads
/// into shared [State].
///
/// Only the originating thread should perform this.
#[instrument]
fn initialize_operation<const NUM_THREADS: usize, const NUM_WORDS: usize>(
    state: &State<NUM_THREADS, NUM_WORDS>,
    thread_index: ThreadIndex,
    kcas_words: [KCasWord; NUM_WORDS],
) -> SequenceNum {
    // store all KCasWords into shared state
    for word_num in 0..kcas_words.len() {
        let kcas_word: &KCasWord = &kcas_words[word_num];

        trace!("Saving target address for word num {word_num} to shared state");
        let target_address: *mut AtomicUsize =
            kcas_word.target_address as *const AtomicUsize as *mut AtomicUsize;
        state.target_addresses[thread_index][word_num].store(target_address, Ordering::Release);

        trace!("Saving expected value for word num {word_num} to shared state");
        state.expected_values[thread_index][word_num]
            .store(kcas_word.expected_value, Ordering::Release);

        trace!("Saving desired value for word num {word_num} to shared state");
        state.desired_values[thread_index][word_num]
            .store(kcas_word.desired_value, Ordering::Release);
    }

    // change the stage to Claiming
    trace!("Loading stage and sequence");
    let original_stage_and_sequence: StageAndSequence =
        state.stage_and_sequence_numbers[thread_index].load(Ordering::Acquire);

    let sequence: SequenceNum =
        extract_sequence_from_stage_and_sequence(original_stage_and_sequence);
    let claiming_stage_and_sequence: StageAndSequence =
        construct_stage_and_sequence(Stage::Claiming, sequence);

    trace!("Saving stage and sequence {claiming_stage_and_sequence} to shared state");
    state.stage_and_sequence_numbers[thread_index]
        .store(claiming_stage_and_sequence, Ordering::Release);
    trace!("Saved stage and sequence {claiming_stage_and_sequence} to shared state");

    sequence
}

/// Change the [StageAndSequence] to indicate the operation has finished and the thread is prepared
/// for the next operation.
///
/// Only the originating thread should perform this.
#[instrument]
fn prepare_for_next_operation<const NUM_THREADS: usize, const NUM_WORDS: usize>(
    thread_state: &State<NUM_THREADS, NUM_WORDS>,
    thread_index: ThreadIndex,
    sequence: SequenceNum,
) {
    // make sure to roll over if we exceed the bits allocated to us
    let next_sequence: SequenceNum =
        sequence + 1 % get_sequence_mask_for_thread_and_sequence::<NUM_THREADS>();
    let next_stage_and_sequence: StageAndSequence =
        construct_stage_and_sequence(Stage::Inactive, next_sequence);

    trace!("Storing stage and sequence {next_stage_and_sequence}");
    thread_state.stage_and_sequence_numbers[thread_index]
        .store(next_stage_and_sequence, Ordering::Release);
}

#[derive(Debug, Display)]
enum HelpError {
    /** While helping thread {thread_id}, its sequence number changed from {original_sequence} to
       {actual_sequence}.
    */
    SequenceChangedWhileHelping {
        thread_id: ThreadId,
        original_sequence: SequenceNum,
        actual_sequence: SequenceNum,
    },

    /** While helping thread {0}, we discovered that it is already at a terminal stage: {1}
       such that there is nothing left to do.
    */
    HelpeeStageIsAlreadyTerminal(ThreadId, Stage),

    /// Encountered a fatal error while helping another thread: {0}
    Fatal(FatalError),
}

impl From<FatalError> for HelpError {
    fn from(fatal_error: FatalError) -> Self {
        Self::Fatal(fatal_error)
    }
}

/// Help another thread perform a KCAS operation.
#[instrument]
fn help_thread<const NUM_THREADS: usize, const NUM_WORDS: usize>(
    shared_state: &State<NUM_THREADS, NUM_WORDS>,
    encountered_thread_and_sequence: ThreadAndSequence,
) -> Result<(), HelpError> {
    let thread_id: ThreadId =
        extract_thread_from_thread_and_sequence::<NUM_THREADS>(encountered_thread_and_sequence);
    let thread_index: ThreadIndex = convert_thread_id_to_thread_index(thread_id);

    let encountered_sequence: SequenceNum =
        extract_sequence_from_thread_and_sequence::<NUM_THREADS>(encountered_thread_and_sequence);

    trace!("Loading stage and sequence so we know how to help thread {thread_id}");
    let stage_and_sequence: StageAndSequence =
        shared_state.stage_and_sequence_numbers[thread_index].load(Ordering::Acquire);
    let stage: Stage = extract_stage_from_stage_and_sequence(stage_and_sequence).map_err(
        |stage_out_of_bounds_error| FatalError::StageOutOfBounds(stage_out_of_bounds_error.0),
    )?;

    let sequence: SequenceNum = extract_sequence_from_stage_and_sequence(stage_and_sequence);
    if sequence != encountered_sequence {
        trace!("sequence {sequence} not equal to the encountered sequence {encountered_sequence}");
        // cannot help an operation which is already finished
        return Err(HelpError::SequenceChangedWhileHelping {
            thread_id,
            original_sequence: encountered_sequence,
            actual_sequence: sequence,
        });
    }
    match stage {
        Stage::Inactive => Err(HelpError::Fatal(FatalError::IllegalHelpeeStage(
            Stage::Inactive,
        ))),
        Stage::Claiming => help_claim_and_transition(
            shared_state,
            thread_index,
            sequence,
            encountered_thread_and_sequence,
        ),
        Stage::Setting => help_set_and_transition(
            shared_state,
            thread_index,
            sequence,
            encountered_thread_and_sequence,
        ),
        Stage::Reverting => help_revert_and_transition(
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
                    Stage::Reverting,
                    Stage::Reverted,
                    || Ok(()),
                )
            },
        ),
        Stage::Successful | Stage::Reverted => {
            Err(HelpError::HelpeeStageIsAlreadyTerminal(thread_id, stage))
        }
    }
}

#[cfg(all(feature = "std", test))]
mod tests {
    use crate::err::{Error, FatalError};
    use crate::kcas::{kcas, KCasWord, State};
    use crate::sync::AtomicUsize;
    use std::sync::atomic::Ordering;
    use test_log::test;
    use tracing::debug;

    #[test]
    fn test_when_all_targets_are_expected_values_kcas_succeeds() {
        let state: State<1, 3> = State::new();

        let first_target: AtomicUsize = AtomicUsize::new(0);
        let second_target: AtomicUsize = AtomicUsize::new(50);
        let third_target: AtomicUsize = AtomicUsize::new(100);

        debug!("first_target before kcas: {first_target:?}");
        debug!("second_target before kcas: {second_target:?}");
        debug!("third_target before kcas: {third_target:?}");

        let kcas_words: [KCasWord; 3] = [
            KCasWord::new(&first_target, 0, 1),
            KCasWord::new(&second_target, 50, 51),
            KCasWord::new(&third_target, 100, 101),
        ];

        let result: Result<(), Error> = kcas(&state, 1, kcas_words);
        debug!("kcas result: {result:?}");
        debug!("first_target after kcas: {first_target:?}");
        debug!("second_target after kcas: {second_target:?}");
        debug!("third_target after kcas: {third_target:?}");

        assert!(result.is_ok());
        assert_eq!(first_target.load(Ordering::Acquire), 1);
        assert_eq!(second_target.load(Ordering::Acquire), 51);
        assert_eq!(third_target.load(Ordering::Acquire), 101);
    }

    #[test]
    fn test_when_a_target_is_an_unexpected_value_kcas_fails() {
        let state: State<1, 3> = State::new();

        let first_target: AtomicUsize = AtomicUsize::new(0);
        let second_target: AtomicUsize = AtomicUsize::new(50);
        let third_target: AtomicUsize = AtomicUsize::new(100);

        debug!("first_target before kcas: {first_target:?}");
        debug!("second_target before kcas: {second_target:?}");
        debug!("third_target before kcas: {third_target:?}");

        let kcas_words: [KCasWord; 3] = [
            KCasWord::new(&first_target, 0, 51),
            KCasWord::new(&second_target, 50, 51),
            // this should fail the operation
            KCasWord::new(&third_target, 99, 101),
        ];

        let result: Result<(), Error> = kcas(&state, 1, kcas_words);
        debug!("kcas result: {result:?}");
        debug!("first_target after kcas: {first_target:?}");
        debug!("second_target after kcas: {second_target:?}");
        debug!("third_target after kcas: {third_target:?}");

        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), Error::ValueWasNotExpectedValue);
    }

    #[test]
    fn test_when_a_value_is_all_1s_kcas_succeeds() {
        let state: State<1, 2> = State::new();

        let first_target: AtomicUsize = AtomicUsize::new(0);
        let second_target: AtomicUsize = AtomicUsize::new(usize::MAX);

        debug!("first_target before kcas: {first_target:?}");
        debug!("second_target before kcas: {second_target:?}");

        let kcas_words: [KCasWord; 2] = [
            KCasWord::new(&first_target, 0, 1),
            KCasWord::new(&second_target, usize::MAX, usize::MAX - 1),
        ];

        let result: Result<(), Error> = kcas(&state, 1, kcas_words);
        debug!("kcas result: {result:?}");
        debug!("first_target after kcas: {first_target:?}");
        debug!("second_target after kcas: {second_target:?}");

        assert!(result.is_ok());
        assert_eq!(first_target.load(Ordering::Acquire), 1);
        assert_eq!(second_target.load(Ordering::Acquire), usize::MAX - 1);
    }

    #[test]
    fn test_when_a_value_violates_thread_id_space_kcas_fails() {
        let state: State<4, 2> = State::new();

        let illegal_value: usize = 5usize << (usize::BITS - 3);
        let first_target: AtomicUsize = AtomicUsize::new(0);
        let second_target: AtomicUsize = AtomicUsize::new(illegal_value);

        debug!("first_target before kcas: {first_target:?}");
        debug!("second_target before kcas: {second_target:?}");

        let kcas_words: [KCasWord; 2] = [
            KCasWord::new(&first_target, 0, 1),
            KCasWord::new(&second_target, 0, 1),
        ];

        let result: Result<(), Error> = kcas(&state, 1, kcas_words);
        debug!("kcas result: {result:?}");
        debug!("first_target after kcas: {first_target:?}");
        debug!("second_target after kcas: {second_target:?}");

        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            Error::Fatal(e) if matches!(e, FatalError::TopBitsOfValueWereIllegal { .. })
        ));
    }
}
