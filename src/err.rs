use core::fmt::{Arguments, Display, Formatter, Pointer, Write};
use crate::stage::Stage;
use crate::aliases::{SequenceNum, ThreadId};

#[derive(Debug, Eq, PartialEq)]
pub enum KCasError {
    FatalInternalError(FatalError),
    ValueWasNotExpectedValue,
    HadToHelpThread,
}

#[derive(Debug, Eq, PartialEq)]
pub enum FatalError {
    IllegalStageTransition { original_stage: Stage, next_stage: Stage },
    ThreadSequenceModifiedByAnotherThread { thread_id: ThreadId },
    OutOfBoundsStage(usize),
    CASFailedWhileFinalizingSlots,
    IllegalState,
    InvalidPointer,
}

impl Display for FatalError {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        match self {
            FatalError::IllegalStageTransition { original_stage, next_stage } => {
                f.write_fmt(format_args!("Stage illegally transitioned from {} to {}", original_stage, next_stage))
            }
            FatalError::ThreadSequenceModifiedByAnotherThread { thread_id } => {
                f.write_fmt(format_args!("Thread sequence for thread {} was changed by another thread, which is not supposed to be possible", thread_id))
            }
            FatalError::OutOfBoundsStage(invalid_stage) => {
                f.write_fmt(format_args!("Attempted to deserialize a stage which is out of bounds: {}", invalid_stage))
            }
            FatalError::CASFailedWhileFinalizingSlots => {
                f.write_fmt(format_args!("CAS failed while finalizing a slot which was already acquired"))
            }
            FatalError::IllegalState => {
                f.write_fmt(format_args!("Invalid state"))
            }
            FatalError::InvalidPointer => {
                f.write_fmt(format_args!("A pointer was invalid"))
            }
        }
    }
}
impl From<FatalError> for KCasError {
    fn from(value: FatalError) -> Self {
        KCasError::FatalInternalError(value)
    }
}

// impl<'a> Display for KCasError<'a> {
//     fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
//         match self {
//             Self::FatalInternalError(arguments) => f.write_fmt(*arguments)
//         }
//     }
// }

// impl<'a> Into<KCasError<'a>> for ConcurrentChangeError {
//     fn into(self) -> KCasError<'a> {
//         match self {
//             ConcurrentChangeError::StageChanged { current_word_num, current_stage } => {
//
//             }
//             ConcurrentChangeError::SequenceChanged { .. } => {}
//             ConcurrentChangeError::StageBecameInvalid(_) => {}
//         }
//     }
// }

#[derive(Debug, Eq, PartialEq)]
pub struct StageOutOfBoundsError(pub(crate) usize);

impl From<StageOutOfBoundsError> for FatalError {
    fn from(value: StageOutOfBoundsError) -> Self {
        FatalError::OutOfBoundsStage(value.0)
    }
}

impl Display for StageOutOfBoundsError {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        let a = format_args!("Attempted to parse {} as a stage, but it is not a valid stage", self.0);
        f.write_fmt(format_args!("Attempted to parse {} as a stage, but it is not a valid stage", self.0))
    }
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum ConcurrentChangeError {
    StageChanged { current_word_num: usize, current_stage: Stage },
    SequenceChanged { current_word_num: usize, current_sequence: SequenceNum },
    StageBecameInvalid(StageOutOfBoundsError)
}

impl From<StageOutOfBoundsError> for ConcurrentChangeError {
    fn from(invalid_stage_error: StageOutOfBoundsError) -> Self {
        Self::StageBecameInvalid(invalid_stage_error)
    }
}

impl Display for ConcurrentChangeError {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        match self {
            ConcurrentChangeError::StageChanged { current_stage, .. } => {
                f.write_fmt(format_args!("The stage changed to {} while performing an operation", current_stage))
            }
            ConcurrentChangeError::SequenceChanged { current_sequence, .. } => {
                f.write_fmt(format_args!("The sequence changed to {} while performing an operation", current_sequence))
            }
            ConcurrentChangeError::StageBecameInvalid(invalid_stage_error) => {
                f.write_fmt(format_args!("The stage became invalid while performing an operation: {}", invalid_stage_error))
            }
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum ClaimError {
    ConcurrentChangeError(ConcurrentChangeError),
    ValueWasDifferentThread { current_word_num: usize, other_thread_id: ThreadId, other_sequence_num: SequenceNum },
    ValueWasNotExpectedValue { current_word_num: usize, actual_value: usize },
    InvalidPointer,
}

impl Display for ClaimError {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        match self {
            ClaimError::ConcurrentChangeError(concurrent_change_error) => {
                f.write_fmt(format_args!("State changed unexpectedly while acquiring slots: {}", concurrent_change_error))
            }
            ClaimError::ValueWasDifferentThread { other_thread_id, other_sequence_num, .. } => {
                f.write_fmt(format_args!("Could not acquire slot because it contained a marker for thread {} and sequence {}", other_thread_id, other_sequence_num))
            }
            ClaimError::ValueWasNotExpectedValue { actual_value, .. } => {
                f.write_fmt(format_args!("Could not acquire slot because the actual value was not the expected value: {}", actual_value))
            }
            ClaimError::InvalidPointer => {
                f.write_str("Could not acquire slot because a pointer was invalid")
            }
        }
    }
}

impl From<ConcurrentChangeError> for ClaimError {
    fn from(concurrent_change_error: ConcurrentChangeError) -> Self {
        Self::ConcurrentChangeError(concurrent_change_error)
    }
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum SetError {
    ConcurrentChangeError(ConcurrentChangeError),
    IllegalState,
    InvalidPointer,
}

impl Display for SetError {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        match self {
            SetError::ConcurrentChangeError(concurrent_change_error) => {
                f.write_fmt(format_args!("Encountered a concurrent state change while setting slots to desired values: {}", concurrent_change_error))
            }
            SetError::IllegalState => {
                f.write_fmt(format_args!("Encountered a "))
            }
            SetError::InvalidPointer => {
                f.write_fmt(format_args!("Encountered a "))
            }
        }
    }
}

impl From<ConcurrentChangeError> for SetError {
    fn from(concurrent_change_error: ConcurrentChangeError) -> Self {
        Self::ConcurrentChangeError(concurrent_change_error)
    }
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum RevertError {
    ConcurrentChangeError(ConcurrentChangeError),
    InvalidPointer,
}

impl From<ConcurrentChangeError> for RevertError {
    fn from(concurrent_change_error: ConcurrentChangeError) -> Self {
        Self::ConcurrentChangeError(concurrent_change_error)
    }
}
