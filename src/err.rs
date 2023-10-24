use crate::types::{Stage, ThreadId, WordNum};
use displaydoc::Display;

/// Any error which can occur during a k-CAS operation.
#[derive(Debug, Display, Eq, PartialEq)]
pub enum Error {
    /// An unrecoverable error occurred and in-flight changes may not have been cleaned up: {0}
    Fatal(FatalError),
    /// The value at one of the target addresses was not equal to the expected value.
    ValueWasNotExpectedValue,
}

impl From<FatalError> for Error {
    fn from(fatal_error: FatalError) -> Self {
        Error::Fatal(fatal_error)
    }
}

/// An unrecoverable error.
#[derive(Debug, Display, Eq, PartialEq)]
pub enum FatalError {
    /// Tried to deserialize a number as a stage, but it does not correlate to a valid stage: {0}
    StageOutOfBounds(usize),

    /// Stage changed from {original_stage} to {actual_stage}, which should not be possible.
    IllegalStageChange {
        original_stage: Stage,
        actual_stage: Stage,
    },

    /// While helping a thread, that thread's stage changed to {0}, which should not be possible.
    IllegalHelpeeStage(Stage),

    /// The target address {target_address} at word number {word_num} was not a valid pointer
    TargetAddressWasNotValidPointer {
        word_num: WordNum,
        target_address: usize,
    },

    /** The sequence number of the current KCAS operation was changed by a thread other than
        the originating thread {originating_thread_id}, which should not be possible.
    */
    SequenceNumChangedByNonOriginatingThread { originating_thread_id: ThreadId },

    /** While setting the target address {target_address} at word number {word_num} to the desired
        value, a CAS failure indicated the current value at the address was not a claim marker, but
        instead {actual_value}, which should not be possible.
    */
    TriedToSetValueWhichWasNotClaimMarker {
        word_num: WordNum,
        target_address: usize,
        actual_value: usize,
    },
    /** The top {num_reserved_bits} bits of value {value} at target address {target_address} with
        word number {word_num} were not expected. These bits are reserved for kcas internal thread
        identifiers and should not be used for real information: the only allowed values are either
        all 0s or all 1s.
    */
    TopBitsOfValueWereIllegal {
        word_num: WordNum,
        target_address: usize,
        value: usize,
        /// the most significant bits
        num_reserved_bits: usize,
    },
    ///
    SharedStateWasNotValidPointer,
}

impl From<StageOutOfBoundsError> for FatalError {
    fn from(stage_out_of_bounds_error: StageOutOfBoundsError) -> Self {
        FatalError::StageOutOfBounds(stage_out_of_bounds_error.0)
    }
}

/// Attempted to convert a usize into a Stage but it was out of bounds: {0}
#[derive(Debug, Display, Eq, PartialEq)]
pub(crate) struct StageOutOfBoundsError(pub(crate) usize);
