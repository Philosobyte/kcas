use core::fmt::{Display, Formatter, Pointer, Write};
use crate::stage::Stage;
use crate::types::{SequenceNumber, ThreadId};

#[derive(Debug, Eq, PartialEq)]
pub enum KCasError<'a> {
    InternalError(&'a str)
}

impl<'a> Display for KCasError<'a> {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::InternalError(s) => f.write_str(s)
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct InvalidStageError(usize);

impl Display for InvalidStageError {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        f.write_fmt(format_args!("Attempted to parse {} as a stage, but it is not a valid stage", self.0))
    }
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum ConcurrentChangeError {
    StageChanged { current_word_num: usize, current_stage: Stage },
    SequenceChanged { current_word_num: usize, current_sequence: SequenceNumber },
    StageBecameInvalid(InvalidStageError)
}

impl From<InvalidStageError> for ConcurrentChangeError {
    fn from(invalid_stage_error: InvalidStageError) -> Self {
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
pub(crate) enum AcquireError {
    ConcurrentChangeError(ConcurrentChangeError),
    ValueWasDifferentThread { current_word_num: usize, other_thread_id: ThreadId, other_sequence_num: SequenceNumber },
    ValueWasNotExpectedValue { current_word_num: usize, actual_value: usize },
    InvalidPointer,
}

impl Display for AcquireError {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        match self {
            AcquireError::ConcurrentChangeError(concurrent_change_error) => {
                f.write_fmt(format_args!("State changed unexpectedly while acquiring slots: {}", concurrent_change_error))
            }
            AcquireError::ValueWasDifferentThread { other_thread_id, other_sequence_num, .. } => {
                f.write_fmt(format_args!("Could not acquire slot because it contained a marker for thread {} and sequence {}", other_thread_id, other_sequence_num))
            }
            AcquireError::ValueWasNotExpectedValue { actual_value, .. } => {
                f.write_fmt(format_args!("Could not acquire slot because the actual value was not the expected value: {}", actual_value))
            }
            AcquireError::InvalidPointer => {
                f.write_str("Could not acquire slot because a pointer was invalid")
            }
        }
    }
}

impl From<ConcurrentChangeError> for AcquireError {
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
            SetError::InvalidPointer => {}
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
