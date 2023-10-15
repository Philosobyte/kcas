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

#[cfg(feature = "tracing")]
use tracing::instrument;

mod claim;
mod revert;
mod set;
mod stage_change;

/// A structure containing all the information needed to perform a single CAS operation.
///
/// `target_address` is the mutable reference whose value should be CASed. Values `expected_element`
/// and `desired_element` can be any usize, including a thin pointer, as long as the most
/// significant bits are either all 0s or all 1s. The number of bits which must follow this rule is
/// determined by [fn@crate::types::get_bit_length_of_num_threads] at compile time depending on the
/// number of desired threads for performing KCAS operations. These bits are used internally by
/// kcas to differentiate between temporary [ThreadAndSequence] markers and real values.
///
/// The reason the most significant bits in values are allowed to be either all 0s or all 1s is to
/// allow sign extension, which is required by canonical pointers in some operating systems.
#[derive(Debug)]
pub struct KCasWord<'a> {
    target_address: &'a AtomicUsize,
    expected_element: usize,
    desired_element: usize,
}

impl<'a> KCasWord<'a> {
    pub fn new(
        target_address: &'a AtomicUsize,
        expected_element: usize,
        desired_element: usize,
    ) -> Self {
        Self {
            target_address,
            expected_element,
            desired_element,
        }
    }
}

/// Holds KCAS operation state shared between all threads. This allows threads to help each other.
///
/// [NUM_THREADS] is the maximum number of threads allowed to perform KCAS operations at
/// any given point in time. Memory allocation at initialization grows linearly with [NUM_THREADS].
/// Besides wasted memory allocation, it is okay to underutilize the number of threads.
///
/// [NUM_WORDS] is the number of [KCasWord]s to CAS during every operation. Memory allocation
/// at initialization grows linearly with [NUM_WORDS], just like [NUM_THREADS]. It is currently
/// expected that each KCAS operation involves exactly [NUM_WORDS] words, although support for
/// supplying fewer than [NUM_WORDS] words is planned.
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
#[cfg_attr(feature = "tracing", instrument)]
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
    claim_and_transition(shared_state, thread_index, sequence, thread_and_sequence)
}

/// Persist information about this operation at [ThreadIndex] which may be needed by other threads
/// into shared [State].
///
/// Only the originating thread should perform this.
#[cfg_attr(feature = "tracing", instrument)]
fn initialize_operation<const NUM_THREADS: usize, const NUM_WORDS: usize>(
    state: &State<NUM_THREADS, NUM_WORDS>,
    thread_index: ThreadIndex,
    mut kcas_words: [KCasWord; NUM_WORDS],
) -> SequenceNum {
    // store all KCasWords into shared state
    for row_num in 0..kcas_words.len() {
        let kcas_word: &KCasWord = &kcas_words[row_num];

        let target_address: *mut AtomicUsize =
            kcas_word.target_address as *const AtomicUsize as *mut AtomicUsize;
        state.target_addresses[thread_index][row_num].store(target_address, Ordering::Release);

        state.expected_values[thread_index][row_num]
            .store(kcas_word.expected_element, Ordering::Release);
        state.desired_values[thread_index][row_num]
            .store(kcas_word.desired_element, Ordering::Release);
    }

    // change the stage to Claiming
    let original_stage_and_sequence: StageAndSequence =
        state.stage_and_sequence_numbers[thread_index].load(Ordering::Acquire);
    let sequence: SequenceNum =
        extract_sequence_from_stage_and_sequence(original_stage_and_sequence);

    let claiming_stage_and_sequence: StageAndSequence =
        construct_stage_and_sequence(Stage::Claiming, sequence);
    state.stage_and_sequence_numbers[thread_index]
        .store(claiming_stage_and_sequence, Ordering::Release);

    sequence
}

/// Change the [StageAndSequence] to indicate the operation has finished and the thread is prepared
/// for the next operation.
///
/// Only the originating thread should perform this.
#[cfg_attr(feature = "tracing", instrument)]
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
        actual_sequence: SequenceNum
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
#[cfg_attr(feature = "tracing", instrument)]
fn help_thread<const NUM_THREADS: usize, const NUM_WORDS: usize>(
    shared_state: &State<NUM_THREADS, NUM_WORDS>,
    encountered_thread_and_sequence: ThreadAndSequence,
) -> Result<(), HelpError> {
    let thread_id: ThreadId =
        extract_thread_from_thread_and_sequence::<NUM_THREADS>(encountered_thread_and_sequence);
    let thread_index: ThreadIndex = convert_thread_id_to_thread_index(thread_id);

    let encountered_sequence: SequenceNum =
        extract_sequence_from_thread_and_sequence::<NUM_THREADS>(encountered_thread_and_sequence);

    // we need to know what stage the thread is at so we know how to help
    let stage_and_sequence: StageAndSequence =
        shared_state.stage_and_sequence_numbers[thread_index].load(Ordering::Acquire);
    let stage: Stage = extract_stage_from_stage_and_sequence(stage_and_sequence).map_err(
        |stage_out_of_bounds_error| FatalError::StageOutOfBounds(stage_out_of_bounds_error.0),
    )?;

    let sequence: SequenceNum = extract_sequence_from_stage_and_sequence(stage_and_sequence);
    if sequence != encountered_sequence {
        // cannot help an operation which is already finished
        return Err(HelpError::SequenceChangedWhileHelping { thread_id, original_sequence: encountered_sequence, actual_sequence: sequence });
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
        Stage::Successful | Stage::Reverted => Err(HelpError::HelpeeStageIsAlreadyTerminal(thread_id, stage)),
    }
}

// #[cfg(test)]
// #[cfg(loom)]
// mod loom_tests {
//     use crate::err::KCasError;
//     use crate::kcas::{kcas, KCasWord, SharedState};
//
//     use loom::thread;
//
//     extern crate std;
//
//     #[test]
//     fn test() {
//         loom::model(|| {
//
//         });
//     }
//
// }

#[cfg(test)]
#[cfg(feature = "std")]
mod tests {
    use tracing::debug;
    use test_log::test;
    use crate::err::Error;
    use crate::kcas::{kcas, KCasWord, State};
    use crate::sync::AtomicUsize;

    #[test]
    fn test_all_targets_are_expected_values() {
        let state: State<1, 3> = State::new();

        let first_location: AtomicUsize = AtomicUsize::new(50);
        let second_location: AtomicUsize = AtomicUsize::new(70);
        let third_location: AtomicUsize = AtomicUsize::new(100);
        debug!("first_location before kcas: {first_location:?}");
        debug!("second_location before kcas: {second_location:?}");
        debug!("third_location before kcas: {third_location:?}");

        let kcas_words: [KCasWord; 3] = [
            KCasWord::new(&first_location, 50, 51),
            KCasWord::new(&second_location, 70, 71),
            KCasWord::new(&third_location, 100, 101),
        ];

        assert!(kcas(&state, 1, kcas_words).is_ok());

        debug!("first_location after kcas: {first_location:?}");
        debug!("second_location after kcas: {second_location:?}");
        debug!("third_location after kcas: {third_location:?}");

        let first_location_address_v2: &AtomicUsize = &first_location;
        let second_location_address_v2: &AtomicUsize = &second_location;
        let third_location_address_v2: &AtomicUsize = &third_location;

        let kcas_words: [KCasWord; 3] = [
            KCasWord::new(first_location_address_v2, 51, 52),
            KCasWord::new(second_location_address_v2, 71, 72),
            KCasWord::new(third_location_address_v2, 101, 102),
        ];

        assert!(kcas(&state, 1, kcas_words).is_ok());
    }

    #[test]
    fn test_a_target_is_unexpected_value() {
        let state: State<1, 3> = State::new();

        let first_location: AtomicUsize = AtomicUsize::new(50);
        let second_location: AtomicUsize = AtomicUsize::new(70);
        let third_location: AtomicUsize = AtomicUsize::new(90);
        debug!("first_location before kcas: {first_location:?}");
        debug!("second_location before kcas: {second_location:?}");
        debug!("third_location before kcas: {third_location:?}");

        let kcas_words: [KCasWord; 3] = [
            KCasWord::new(&first_location, 50, 51),
            KCasWord::new(&second_location, 70, 71),
            KCasWord::new(&third_location, 100, 101),
        ];

        let error: Error = kcas(&state, 1, kcas_words).unwrap_err();
        assert!(matches!(error, Error::ValueWasNotExpectedValue));
    }
    //
    // #[test]
    // fn test_if_drop_is_called_with_arc() {
    //     let shared_state: SharedState<1, 3> = SharedState::new();
    //     println!("Shared state after initialization: {:?}", shared_state);
    //     // let shared_state_pointer: *mut SharedState<1, 3> = &shared_state;
    //
    //     let shared_state_arc: Arc<SharedState<1, 3>> = Arc::new(shared_state);
    //     let shared_state_arc_clone:  Arc<SharedState<1, 3>> = shared_state_arc.clone();
    //     let join_handle: std::thread::JoinHandle<()> = std::thread::spawn(move || {
    //         println!("inside child thread now");
    //         // let inner_shared_state: &SharedState<1, 3> = shared_state_pointer.as_ref().unwrap();
    //         let inner_shared_state: &SharedState<1, 3> = shared_state_arc_clone.as_ref();
    //         println!("Obtained reference to shared state using a pointer: {:?}", inner_shared_state);
    //     });
    //
    //     join_handle.join();
    //     println!("test method finishing");
    // }
    //
    // #[test]
    // fn test_if_drop_is_called_with_raw_pointer() {
    //     let shared_state: SharedState<1, 3> = SharedState::new();
    //     println!("Shared state after initialization: {:?}", shared_state);
    //     let shared_state_pointer: *mut SharedState<1, 3> = &shared_state;
    //     let shared_state_ref: &SharedState<1, 3> = unsafe { shared_state_pointer.as_ref() }.unwrap();
    //     println!("Shared state ref: {:?}", shared_state_ref);
    //     println!("test method finishing");
    // }
    //
    // #[test]
    // fn test_unsafe_wrapper_with_raw_pointer() {
    //     let shared_state: SharedState<3, 3> = SharedState::new();
    //
    //     let first_wrapper: UnsafeKCasStateWrapper<3, 3> = UnsafeKCasStateWrapper::construct(&shared_state).unwrap();
    //     println!("first wrapper: {:?}", first_wrapper);
    //
    //     let second_wrapper: UnsafeKCasStateWrapper<3, 3> = UnsafeKCasStateWrapper::construct(&shared_state).unwrap();
    //     println!("second wrapper: {:?}", second_wrapper);
    //
    //     {
    //         let third_wrapper: UnsafeKCasStateWrapper<3, 3> = UnsafeKCasStateWrapper::construct(&shared_state).unwrap();
    //         println!("third wrapper: {:?}", third_wrapper);
    //     }
    //
    //     let fourth_wrapper: UnsafeKCasStateWrapper<3, 3> = UnsafeKCasStateWrapper::construct(&shared_state).unwrap();
    //     println!("fourth wrapper: {:?}", fourth_wrapper);
    // }

    // #[test]
    // fn test_multiple_threads() {
    //     let shared_state: SharedState<1, 3> = SharedState::new();
    //     thread::spawn(|| {
    //
    //     });
    //     let mut first_location: usize = 50;
    //     let mut second_location: usize = 70;
    //     let mut third_location: usize = 100;
    //     println!("Initial first_location: {first_location}");
    //     println!("Initial second_location: {second_location}");
    //     println!("Initial third_location: {third_location}");
    //     let kcas_words: [KCasWord; 3] = [
    //         KCasWord::new(&mut first_location, 50, 51),
    //         KCasWord::new(&mut second_location, 70, 71),
    //         KCasWord::new(&mut third_location, 100, 101),
    //     ];
    //     let error: KCasError = kcas(&shared_state, 1, kcas_words)
    //         .unwrap_err();
    //     assert!(matches!(error, KCasError::ValueWasNotExpectedValue));
    //
    //     println!("After KCAS first_location: {first_location}");
    //     println!("After KCAS second_location: {second_location}");
    //     println!("After KCAS third_location: {third_location}");
    // }

    #[test]
    fn test_stuff() {
        let (x, _) = (3, 4);

    }
}
