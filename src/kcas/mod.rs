use core::sync::atomic::AtomicBool;
use crate::sync::{AtomicPtr, AtomicUsize, Ordering};
use crate::aliases::{SequenceNum, StageAndSequence, thread_id_to_thread_index, thread_index_to_thread_id, ThreadAndSequence, ThreadId, ThreadIndex};
use crate::err::{FatalError, KCasError, StageOutOfBoundsError};
use crate::kcas::claim::{claim_and_transition, help_claim_and_transition};
use crate::kcas::revert::help_revert_and_transition;
use crate::stage::{Stage, STATUS_BIT_LENGTH};
use crate::kcas::set::help_set_and_transition;
use crate::kcas::stage_change::help_change_stage_and_transition;

#[cfg(feature = "alloc")]
use alloc::sync::Arc;

mod set;
mod revert;
mod claim;
mod stage_change;

/// A structure containing all the information needed to perform a single CAS operation.
///
/// `target_address` is expected to be the mutable reference whose value should be changed
#[derive(Debug, Eq, PartialEq)]
pub struct KCasWord {
    target_address: usize,
    expected_element: usize,
    desired_element: usize,
}

impl KCasWord {
    pub fn new(target_address: usize, expected_element: usize, desired_element: usize) -> Self {
        Self { target_address, expected_element, desired_element }
    }
}

#[derive(Debug)]
pub enum UnsafeKCasStateError {
    AttemptedToInitializeWithInvalidSharedState,
    NoThreadIdsAvailable,
}

#[derive(Debug)]
pub struct UnsafeKCasStateWrapper<const NUM_THREADS: usize, const NUM_WORDS: usize> {
    shared_state: *const SharedState<NUM_THREADS, NUM_WORDS>,
    thread_id: usize,
}

impl<const NUM_THREADS: usize, const NUM_WORDS: usize> UnsafeKCasStateWrapper<NUM_THREADS, NUM_WORDS> {
    pub fn construct(shared_state: *const SharedState<NUM_THREADS, NUM_WORDS>) -> Result<Self, UnsafeKCasStateError> {
        let shared_state_ref = unsafe { shared_state.as_ref() }.ok_or(UnsafeKCasStateError::AttemptedToInitializeWithInvalidSharedState)?;
        let thread_index: ThreadIndex = 'a: {
            for i in 0..NUM_THREADS {
                if shared_state_ref.thread_index_slots[i].compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire).is_ok() {
                    shared_state_ref.num_threads_in_use.fetch_add(1, Ordering::Acquire);
                    break 'a i;
                }
            }
            return Err(UnsafeKCasStateError::NoThreadIdsAvailable);
        };
        Ok(
            Self {
                shared_state, thread_id: thread_index_to_thread_id(thread_index)
            }
        )
    }
}

impl<const NUM_THREADS: usize, const NUM_WORDS: usize> Drop for UnsafeKCasStateWrapper<NUM_THREADS, NUM_WORDS> {
    fn drop(&mut self) {
        let thread_index: ThreadIndex = thread_id_to_thread_index(self.thread_id);

        #[cfg(test)]
        #[cfg(feature = "std")]
        println!("Dropping for thread with id: {} and index: {}", self.thread_id, thread_index);

        let shared_state_ref = match unsafe { self.shared_state.as_ref() } {
            None => return,
            Some(shared_state) => shared_state,
        };
        shared_state_ref.thread_index_slots[thread_index].store(false, Ordering::Release);
        shared_state_ref.num_threads_in_use.fetch_min(1, Ordering::AcqRel);
    }
}

pub enum KCasStateError {
    NoThreadIdsAvailable,
}

#[derive(Debug)]
#[cfg(feature = "alloc")]
pub struct KCasStateWrapper<const NUM_THREADS: usize, const NUM_WORDS: usize> {
    shared_state: Arc<SharedState<NUM_THREADS, NUM_WORDS>>,
    thread_id: usize,
}

#[cfg(feature = "alloc")]
impl<const NUM_THREADS: usize, const NUM_WORDS: usize> KCasStateWrapper<NUM_THREADS, NUM_WORDS> {
    pub fn construct(shared_state: Arc<SharedState<NUM_THREADS, NUM_WORDS>>) -> Result<Self, KCasStateError> {
        let shared_state_ref: &SharedState<NUM_THREADS, NUM_WORDS> = shared_state.as_ref();

        let thread_index: ThreadIndex = 'a: {
            for i in 0..NUM_THREADS {
                if shared_state_ref.thread_index_slots[i].compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire).is_ok() {
                    shared_state_ref.num_threads_in_use.fetch_add(1, Ordering::Acquire);
                    break 'a i;
                }
            }
            return Err(KCasStateError::NoThreadIdsAvailable);
        };
        Ok(
            Self {
                shared_state, thread_id: thread_index_to_thread_id(thread_index)
            }
        )
    }
}

#[cfg(feature = "alloc")]
impl<const NUM_THREADS: usize, const NUM_WORDS: usize> Drop for KCasStateWrapper<NUM_THREADS, NUM_WORDS> {
    fn drop(&mut self) {
        let thread_index: ThreadIndex = thread_id_to_thread_index(self.thread_id);

        #[cfg(test)]
        #[cfg(feature = "std")]
        println!("Dropping for thread with id: {} and index: {}", self.thread_id, thread_index);

        let shared_state_ref: &SharedState<NUM_THREADS, NUM_WORDS> = self.shared_state.as_ref();
        shared_state_ref.thread_index_slots[thread_index].store(false, Ordering::Release);
        shared_state_ref.num_threads_in_use.fetch_min(1, Ordering::AcqRel);
    }
}

#[derive(Debug)]
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

    num_threads_in_use: AtomicUsize,
    thread_index_slots: [AtomicBool; NUM_THREADS],

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
            num_threads_in_use: AtomicUsize::default(),
            thread_index_slots: core::array::from_fn(|_| AtomicBool::default()),
            target_addresses: core::array::from_fn(|_| core::array::from_fn(|_| AtomicPtr::default())),
            expected_elements: core::array::from_fn(|_| core::array::from_fn(|_| AtomicUsize::default())),
            desired_elements: core::array::from_fn(|_| core::array::from_fn(|_| AtomicUsize::default())),
            stage_and_sequence_numbers: core::array::from_fn(|_| AtomicUsize::default()),
        }
    }
}

#[cfg(test)]
#[cfg(feature = "std")]
impl<const NUM_THREADS: usize, const NUM_WORDS: usize> Drop for SharedState<NUM_THREADS, NUM_WORDS> {
    fn drop(&mut self) {
        println!("Dropping SharedState");
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
    use alloc::sync::Arc;
    use crate::err::KCasError;
    use crate::kcas::{kcas, KCasWord, SharedState, UnsafeKCasStateWrapper};

    #[test]
    fn test_all_targets_are_expected_value() {
        let shared_state: SharedState<1, 3> = SharedState::new();
        let mut first_location: usize = 50;
        let mut second_location: usize = 70;
        let mut third_location: usize = 100;
        println!("Initial first_location: {first_location}");
        println!("Initial second_location: {second_location}");
        println!("Initial third_location: {third_location}");
        let mut first_location_address: *const usize = &first_location;
        let mut second_location_address: *const usize = &second_location;
        let mut third_location_address: *const usize = &third_location;
        let kcas_words: [KCasWord; 3] = [
            KCasWord::new(first_location_address as usize, 50, 51),
            KCasWord::new(second_location_address as usize, 70, 71),
            KCasWord::new(third_location_address as usize, 100, 101),
        ];

        assert!(kcas(&shared_state, 1, kcas_words).is_ok());

        println!("After KCAS first_location: {first_location}");
        println!("After KCAS second_location: {second_location}");
        println!("After KCAS third_location: {third_location}");
    }

    // #[test]
    // fn test_last_target_is_unexpected_value() {
    //     let shared_state: SharedState<1, 3> = SharedState::new();
    //     let mut first_location: usize = 50;
    //     let mut second_location: usize = 70;
    //     let mut third_location: usize = 99;
    //     println!("Initial first_location: {first_location}");
    //     println!("Initial second_location: {second_location}");
    //     println!("Initial third_location: {third_location}");
    //     let kcas_words: [KCasWord; 3] = [
    //         KCasWord::new(&mut first_location, 50, 51),
    //         KCasWord::new(&mut second_location, 70, 71),
    //         KCasWord::new(&mut third_location, 100, 101),
    //     ];
    //    let error: KCasError = kcas(&shared_state, 1, kcas_words)
    //        .unwrap_err();
    //     assert!(matches!(error, KCasError::ValueWasNotExpectedValue));
    //
    //     println!("After KCAS first_location: {first_location}");
    //     println!("After KCAS second_location: {second_location}");
    //     println!("After KCAS third_location: {third_location}");
    // }
    //
    // #[test]
    // fn test_if_drop_is_called_with_arc() {
    //     let shared_state: SharedState<1, 3> = SharedState::new();
    //     println!("Shared state after initialization: {:?}", shared_state);
    //     // let shared_state_pointer: *const SharedState<1, 3> = &shared_state;
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
    //     let shared_state_pointer: *const SharedState<1, 3> = &shared_state;
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
}
