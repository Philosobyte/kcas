use crate::err::StageOutOfBoundsError;
use core::fmt::{Display, Formatter};

/// The index, from 0 to _k_, of a word in a KCAS operation.
pub(crate) type WordNum = usize;

/// A monotonically increasing counter which identifies a KCAS operation.
///
/// When a KCAS operation completes, the sequence number is incremented in preparation for the next
/// operation.
///
/// The number of bits available for the sequence number depends on the length of a word on the
/// target platform as well as the maximum number of threads configured for any particular
/// [KCasState]. See [ThreadAndSequence] for more information.
pub(crate) type SequenceNum = usize;

/// An identifier for a thread which performs multi-word CAS operations.
///
/// [ThreadId]s start from 1 instead of 0 to help distinguish them from other values which may
/// occupy the same space while serialized.
pub(crate) type ThreadId = usize;

/// A [ThreadId] except it starts from 0 instead of 1.
pub(crate) type ThreadIndex = usize;

pub(crate) fn convert_thread_id_to_thread_index(thread_id: ThreadId) -> ThreadIndex {
    thread_id - 1
}

pub(crate) fn convert_thread_index_to_thread_id(thread_index: ThreadIndex) -> ThreadId {
    thread_index + 1
}

/// The stage of a KCAS operation.
///
/// The stage for a given operation is stored in shared state, keyed by the thread which initiated
/// the operation. Stages provide information to all threads working on an operation about what
/// has already been done and what is currently being done.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Stage {
    /// Indicates there is no KCAS operation in progress.
    Inactive = 0,

    /// Indicates a KCAS operation is in the middle of swapping out expected values at the target
    /// addresses for markers.
    Claiming = 1,

    /// Indicates a KCAS operation has already claimed all of its target addresses and is now
    /// swapping out markers at the target addresses for the desired values.
    Setting = 2,

    /// Indicates a CAS failure occurred while claiming one of the target addresses. The KCAS
    /// operation is now "reverting" or swapping out markers at the target addresses for their
    /// original values.
    Reverting = 3,

    /// Indicates the KCAS operation has succeeded such that all values at the target addresses
    /// were the expected values, and all desired values have been swapped in. As an optimization,
    /// the originating thread does not need to set the operation stage to [enum@Stage::Successful].
    /// Only helper threads set this stage to inform the originating thread what to return.
    Successful = 4,

    /// Indicates the KCAS operation has failed such that one of the values at the target addresses
    /// was not the expected value.As an optimization, the originating thread does not need to set
    /// the operation stage to [enum@Stage::Reverted]. Only helper threads set this stage to inform
    /// the originating thread what to return.
    Reverted = 5,
}

/// The number of bits required to represent a [Stage].
pub(crate) const STAGE_BIT_LENGTH: usize = 3;

impl Display for Stage {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl TryFrom<usize> for Stage {
    type Error = StageOutOfBoundsError;

    fn try_from(stage: usize) -> Result<Self, StageOutOfBoundsError> {
        match stage {
            i if i == Stage::Inactive as usize => Ok(Stage::Inactive),
            i if i == Stage::Claiming as usize => Ok(Stage::Claiming),
            i if i == Stage::Setting as usize => Ok(Stage::Setting),
            i if i == Stage::Reverting as usize => Ok(Stage::Reverting),
            i if i == Stage::Successful as usize => Ok(Stage::Successful),
            i if i == Stage::Reverted as usize => Ok(Stage::Reverted),
            i => Err(StageOutOfBoundsError(i)),
        }
    }
}

/// A `usize` which combines a [Stage] in the 3 most significant bits and a [SequenceNum] in the
/// remaining bits. This allows us to CAS both pieces of information in one operation.
pub(crate) type StageAndSequence = usize;

/// The mask to AND with a [StageAndSequence] in order to extract the [SequenceNum].
pub(crate) const SEQUENCE_MASK_FOR_STAGE_AND_SEQUENCE: usize = {
    let num_sequence_bits: usize = usize::BITS as usize - STAGE_BIT_LENGTH;
    !(0b111 << num_sequence_bits)
};

pub(crate) fn construct_stage_and_sequence(
    stage: Stage,
    sequence: SequenceNum,
) -> StageAndSequence {
    (stage as usize) << (usize::BITS as usize - STAGE_BIT_LENGTH) | sequence
}

pub(crate) fn extract_stage_from_stage_and_sequence(
    stage_and_sequence: StageAndSequence,
) -> Result<Stage, StageOutOfBoundsError> {
    let stage_as_num: usize = stage_and_sequence >> (usize::BITS as usize - STAGE_BIT_LENGTH);
    Stage::try_from(stage_as_num)
}

pub(crate) fn extract_sequence_from_stage_and_sequence(
    stage_and_sequence: StageAndSequence,
) -> SequenceNum {
    stage_and_sequence & SEQUENCE_MASK_FOR_STAGE_AND_SEQUENCE
}

/// A `usize` which stores a [ThreadId] in its most significant bits and a [SequenceNum] in the
/// remaining bits.
///
/// The number of bits allocated to [ThreadId] is determined by [get_bit_length_of_num_threads].
///
/// As `NUM_THREADS` increases, the sequence number allocation decreases. But this should not impact
/// the number of possible KCAS operations because the sequence number will wrap around to 0 once
/// it has reached its maximum value.
pub(crate) type ThreadAndSequence = usize;

/// Determine if a value is a valid KCAS [ThreadAndSequence] marker.
pub const fn is_value_a_kcas_marker<const NUM_THREADS: usize>(value: usize) -> bool {
    let top_bits: usize = extract_thread_from_thread_and_sequence::<NUM_THREADS>(value);
    top_bits > 0 && top_bits <= NUM_THREADS
}

/// Obtain the number of bits needed to store [ThreadId]s for the given number of threads.
///
/// For example, if `number` is 5, then the bit length is 3 because the binary representation
/// of 5 is 101, which consists of 3 bits.
///
/// The number of bits allocated to [ThreadId] depends on the maximum `NUM_THREADS` configured
/// on a [KCasState] instance. Specifically, the number of bits allocated is the bit length of
/// `(NUM_THREADS + 1)`. For example, a `NUM_THREADS` value of 32 means 6 bits are allocated to
/// [ThreadId], since the bit length of (32 + 1) or 33 is 5. On a 64-bit system, the remaining 59
/// bits are allocated to the sequence number.
pub(crate) const fn get_bit_length_of_num_threads<const NUM_THREADS: usize>() -> usize {
    let num_threads_plus_one: usize = NUM_THREADS + 1;
    // can't use a for loop because iterators are not available in constant functions:
    // https://github.com/rust-lang/rust/issues/87575
    let mut i: usize = 0;
    while i != usize::BITS as usize && num_threads_plus_one >> i != 0usize {
        i += 1;
    }
    i
}

/// Construct a mask for [ThreadAndSequence] where the [ThreadId] bits are 0 and the [SequenceNum]
/// bits are 1.
pub(crate) const fn get_sequence_mask_for_thread_and_sequence<const NUM_THREADS: usize>() -> usize {
    let bit_length_of_num_threads: usize = get_bit_length_of_num_threads::<NUM_THREADS>();
    let num_sequence_bits: usize = usize::BITS as usize - bit_length_of_num_threads;
    !(usize::MAX << num_sequence_bits)
}

pub(crate) const fn construct_thread_and_sequence<const NUM_THREADS: usize>(
    thread_id: ThreadId,
    sequence: SequenceNum,
) -> ThreadAndSequence {
    thread_id << (usize::BITS as usize - get_bit_length_of_num_threads::<NUM_THREADS>()) | sequence
}

pub(crate) const fn extract_thread_from_thread_and_sequence<const NUM_THREADS: usize>(
    thread_and_sequence: ThreadAndSequence,
) -> ThreadId {
    thread_and_sequence >> (usize::BITS as usize - get_bit_length_of_num_threads::<NUM_THREADS>())
}

pub(crate) const fn extract_sequence_from_thread_and_sequence<const NUM_THREADS: usize>(
    thread_and_sequence: ThreadAndSequence,
) -> SequenceNum {
    thread_and_sequence & get_sequence_mask_for_thread_and_sequence::<NUM_THREADS>()
}

#[cfg(test)]
#[cfg(feature = "std")]
mod tests {
    use crate::types::{
        construct_stage_and_sequence, extract_sequence_from_stage_and_sequence,
        extract_stage_from_stage_and_sequence, SequenceNum, Stage, StageAndSequence,
        SEQUENCE_MASK_FOR_STAGE_AND_SEQUENCE,
    };
    use test_log::test;
    use tracing::debug;

    #[test]
    fn test() {
        let stage_and_sequence: StageAndSequence =
            construct_stage_and_sequence(Stage::Successful, 0);
        debug!("stage_and_sequence: {stage_and_sequence:?}");
        let stage: Stage =
            extract_stage_from_stage_and_sequence(stage_and_sequence).expect("Could not extract");
        let sequence: SequenceNum = extract_sequence_from_stage_and_sequence(stage_and_sequence);
        debug!("stage: {stage:?}");
        debug!("sequence: {sequence:?}");
        debug!("mask: {:?}", SEQUENCE_MASK_FOR_STAGE_AND_SEQUENCE);
        assert_eq!(stage, Stage::Successful);
        assert_eq!(sequence, 0usize);
    }
}
