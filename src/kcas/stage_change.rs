use crate::err::{Error, FatalError, StageOutOfBoundsError};
use crate::kcas::revert::{help_revert_and_transition, revert_and_transition};
use crate::kcas::set::{help_set_and_transition, set_and_transition};
use crate::kcas::{
    construct_stage_and_sequence, extract_sequence_from_stage_and_sequence,
    extract_stage_from_stage_and_sequence, prepare_for_next_operation, HelpError, State,
};
use crate::sync::Ordering;
use crate::types::{
    convert_thread_index_to_thread_id, SequenceNum, Stage, StageAndSequence, ThreadAndSequence,
    ThreadIndex,
};
use core::fmt::Debug;
use displaydoc::Display;

use tracing::{instrument, trace};

#[derive(Debug, Display)]
pub(super) enum StageAndSequenceVerificationError {
    /// The KCAS operation's stage was changed by another thread to {actual_stage}.
    StageChanged { actual_stage: Stage },
    /// The KCAS operation's sequence number was advanced by another thread to {actual_sequence}.
    SequenceChanged { actual_sequence: SequenceNum },
    /// Tried to deserialize a number as a stage, but it does not correlate to a valid stage: {0}
    StageOutOfBounds(usize),
}

impl From<StageOutOfBoundsError> for StageAndSequenceVerificationError {
    fn from(stage_out_of_bounds_error: StageOutOfBoundsError) -> Self {
        Self::StageOutOfBounds(stage_out_of_bounds_error.0)
    }
}

#[instrument]
pub(super) fn verify_stage_and_sequence_have_not_changed<
    const NUM_THREADS: usize,
    const NUM_WORDS: usize,
>(
    thread_state: &State<NUM_THREADS, NUM_WORDS>,
    thread_index: usize,
    expected_stage: Stage,
    expected_sequence: SequenceNum,
) -> Result<(), StageAndSequenceVerificationError> {
    trace!("thread_index {thread_index}: Loading stage and sequence");
    let actual_stage_and_sequence: StageAndSequence =
        thread_state.stage_and_sequence_numbers[thread_index].load(Ordering::Acquire);

    let actual_sequence: SequenceNum =
        extract_sequence_from_stage_and_sequence(actual_stage_and_sequence);
    let actual_stage: Stage = extract_stage_from_stage_and_sequence(actual_stage_and_sequence)?;

    if actual_sequence != expected_sequence {
        trace!("thread_index {thread_index}: actual sequence {actual_sequence} was not expected sequence {expected_sequence}");
        return Err(StageAndSequenceVerificationError::SequenceChanged { actual_sequence });
    }
    if actual_stage != expected_stage {
        trace!("thread_index {thread_index}: actual stage {actual_stage} was not expected stage {expected_stage}");
        return Err(StageAndSequenceVerificationError::StageChanged { actual_stage });
    }
    Ok(())
}

/// Atomically update the stage in [State] for this operation and, if the update succeeds, call
/// `next_function`.
#[instrument(skip(next_function))]
pub(super) fn change_stage_and_transition<const NUM_THREADS: usize, const NUM_WORDS: usize, F>(
    shared_state: &State<NUM_THREADS, NUM_WORDS>,
    thread_index: ThreadIndex,
    sequence: SequenceNum,
    thread_and_sequence: ThreadAndSequence,
    current_stage: Stage,
    desired_stage: Stage,
    next_function: F,
) -> Result<(), Error>
where
    F: FnOnce() -> Result<(), Error>,
{
    let current_stage_and_sequence: StageAndSequence =
        construct_stage_and_sequence(current_stage.clone(), sequence);
    let desired_stage_and_sequence: StageAndSequence =
        construct_stage_and_sequence(desired_stage, sequence);

    trace!("thread_index {thread_index}: CAS expecting {current_stage_and_sequence} and desiring {desired_stage_and_sequence}");
    let cas_result: Result<usize, usize> = shared_state.stage_and_sequence_numbers[thread_index]
        .compare_exchange(
            current_stage_and_sequence,
            desired_stage_and_sequence,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    if cas_result.is_ok() {
        trace!("thread_index {thread_index}: CAS was ok");
        return next_function();
    }
    let actual_stage_and_sequence: StageAndSequence = cas_result.unwrap_err();
    let actual_stage: Stage = extract_stage_from_stage_and_sequence(actual_stage_and_sequence)
        .map_err(|e| FatalError::from(e))?;
    let actual_sequence: SequenceNum =
        extract_sequence_from_stage_and_sequence(actual_stage_and_sequence);

    if actual_sequence != sequence {
        trace!("thread_index {thread_index}: actual sequence {actual_sequence} was not equal to original sequence {sequence}");
        return revert_and_transition(
            shared_state,
            thread_index,
            thread_and_sequence,
            NUM_WORDS - 1,
            || {
                Err(Error::Fatal(
                    FatalError::SequenceNumChangedByNonOriginatingThread {
                        originating_thread_id: convert_thread_index_to_thread_id(thread_index),
                    },
                ))
            },
        );
    }
    handle_concurrent_stage_change_and_transition(
        shared_state,
        thread_index,
        sequence,
        thread_and_sequence,
        current_stage,
        actual_stage,
    )
}

/// Help another thread update their stage stored in [State] and, if the update is successful, call
/// `next_function`.
#[cfg_attr(feature = "tracing", instrument(skip(next_function)))]
pub(super) fn help_change_stage_and_transition<
    const NUM_THREADS: usize,
    const NUM_WORDS: usize,
    F,
>(
    shared_state: &State<NUM_THREADS, NUM_WORDS>,
    thread_index: ThreadIndex,
    sequence: SequenceNum,
    thread_and_sequence: ThreadAndSequence,
    current_stage: Stage,
    desired_stage: Stage,
    next_function: F,
) -> Result<(), HelpError>
where
    F: FnOnce() -> Result<(), HelpError>,
{
    let current_stage_and_sequence: StageAndSequence =
        construct_stage_and_sequence(current_stage.clone(), sequence);
    let desired_stage_and_sequence: StageAndSequence =
        construct_stage_and_sequence(desired_stage, sequence);

    trace!(
        "thread_index {thread_index}: CAS stage and sequence from {current_stage_and_sequence} to {desired_stage_and_sequence}"
    );
    let cas_result: Result<usize, usize> = shared_state.stage_and_sequence_numbers[thread_index]
        .compare_exchange(
            current_stage_and_sequence,
            desired_stage_and_sequence,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    if cas_result.is_ok() {
        trace!("thread_index {thread_index}: CAS succeeded");
        return next_function();
    }
    let actual_stage_and_sequence: StageAndSequence = cas_result.unwrap_err();
    let actual_stage: Stage = extract_stage_from_stage_and_sequence(actual_stage_and_sequence)
        .map_err(|e| FatalError::from(e))?;
    let actual_sequence: SequenceNum =
        extract_sequence_from_stage_and_sequence(actual_stage_and_sequence);

    if actual_sequence != sequence {
        trace!("thread_index {thread_index}: actual sequence {actual_sequence} was not equal to expected sequence {sequence}");
        return help_revert_and_transition(
            shared_state,
            thread_index,
            thread_and_sequence,
            NUM_WORDS - 1,
            || {
                Err(HelpError::SequenceChangedWhileHelping {
                    thread_id: convert_thread_index_to_thread_id(thread_index),
                    original_sequence: sequence,
                    actual_sequence,
                })
            },
        );
    }
    help_handle_concurrent_stage_change_and_transition(
        shared_state,
        thread_index,
        sequence,
        thread_and_sequence,
        current_stage,
        actual_stage,
    )
}

/// Decide how to proceed upon encountering a concurrent stage or sequence change as the originating
/// thread.
#[instrument]
pub(super) fn handle_concurrent_stage_and_sequence_verification_error_and_transition<
    const NUM_THREADS: usize,
    const NUM_WORDS: usize,
>(
    shared_state: &State<NUM_THREADS, NUM_WORDS>,
    thread_index: ThreadIndex,
    sequence: SequenceNum,
    thread_and_sequence: ThreadAndSequence,
    current_stage: Stage,
    error: StageAndSequenceVerificationError,
) -> Result<(), Error> {
    match error {
        StageAndSequenceVerificationError::StageChanged { actual_stage } => {
            handle_concurrent_stage_change_and_transition(
                shared_state,
                thread_index,
                sequence,
                thread_and_sequence,
                current_stage,
                actual_stage,
            )
        }
        StageAndSequenceVerificationError::SequenceChanged { .. } => Err(Error::Fatal(
            FatalError::SequenceNumChangedByNonOriginatingThread {
                originating_thread_id: convert_thread_index_to_thread_id(thread_index),
            },
        )),
        StageAndSequenceVerificationError::StageOutOfBounds(actual_stage_number) => Err(
            Error::Fatal(FatalError::StageOutOfBounds(actual_stage_number)),
        ),
    }
}

/// Decide how to proceed upon encountering a concurrent stage or sequence change as a helper
/// thread.
#[instrument]
pub(super) fn help_handle_concurrent_stage_and_sequence_verification_error_and_transition<
    const NUM_THREADS: usize,
    const NUM_WORDS: usize,
>(
    state: &State<NUM_THREADS, NUM_WORDS>,
    thread_index: ThreadIndex,
    sequence: SequenceNum,
    thread_and_sequence: ThreadAndSequence,
    current_stage: Stage,
    error: StageAndSequenceVerificationError,
) -> Result<(), HelpError> {
    match error {
        StageAndSequenceVerificationError::StageChanged { actual_stage } => {
            help_handle_concurrent_stage_change_and_transition(
                state,
                thread_index,
                sequence,
                thread_and_sequence,
                current_stage,
                actual_stage,
            )
        }
        StageAndSequenceVerificationError::SequenceChanged { actual_sequence } => {
            help_revert_and_transition(
                state,
                thread_index,
                thread_and_sequence,
                NUM_WORDS - 1,
                || {
                    Err(HelpError::SequenceChangedWhileHelping {
                        thread_id: convert_thread_index_to_thread_id(thread_index),
                        original_sequence: sequence,
                        actual_sequence,
                    })
                },
            )
        }
        StageAndSequenceVerificationError::StageOutOfBounds(actual_stage_number) => Err(
            HelpError::Fatal(FatalError::StageOutOfBounds(actual_stage_number)),
        ),
    }
}

/// Decide how to proceed upon encountering a concurrent stage change as the originating thread.
#[instrument]
pub(super) fn handle_concurrent_stage_change_and_transition<
    const NUM_THREADS: usize,
    const NUM_WORDS: usize,
>(
    shared_state: &State<NUM_THREADS, NUM_WORDS>,
    thread_index: ThreadIndex,
    sequence: SequenceNum,
    thread_and_sequence: ThreadAndSequence,
    current_stage: Stage,
    actual_stage: Stage,
) -> Result<(), Error> {
    match actual_stage {
        // not possible - fatal - don't even bother reverting
        Stage::Inactive | Stage::Claiming => Err(Error::Fatal(FatalError::IllegalStageChange {
            original_stage: current_stage,
            actual_stage,
        })),
        Stage::Setting => {
            set_and_transition(shared_state, thread_index, sequence, thread_and_sequence)
        }
        Stage::Reverting | Stage::Reverted => revert_and_transition(
            shared_state,
            thread_index,
            thread_and_sequence,
            NUM_WORDS - 1,
            || {
                prepare_for_next_operation(shared_state, thread_index, sequence);
                Err(Error::ValueWasNotExpectedValue)
            },
        ),
        Stage::Successful => revert_and_transition(
            shared_state,
            thread_index,
            thread_and_sequence,
            NUM_WORDS - 1,
            || {
                prepare_for_next_operation(shared_state, thread_index, sequence);
                Ok(())
            },
        ),
    }
}

/// Decide how to proceed upon encountering a concurrent stage change as a helper thread.
#[instrument]
pub(super) fn help_handle_concurrent_stage_change_and_transition<
    const NUM_THREADS: usize,
    const NUM_WORDS: usize,
>(
    shared_state: &State<NUM_THREADS, NUM_WORDS>,
    thread_index: ThreadIndex,
    sequence: SequenceNum,
    thread_and_sequence: ThreadAndSequence,
    current_stage: Stage,
    actual_stage: Stage,
) -> Result<(), HelpError> {
    match actual_stage {
        // not possible - fatal - don't even bother reverting
        Stage::Inactive | Stage::Claiming => {
            Err(HelpError::Fatal(FatalError::IllegalStageChange {
                original_stage: current_stage,
                actual_stage,
            }))
        }
        Stage::Setting => {
            help_set_and_transition(shared_state, thread_index, sequence, thread_and_sequence)
        }
        Stage::Reverting => help_revert_and_transition(
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
                    Stage::Reverting,
                    Stage::Reverted,
                    || Ok(()),
                )
            },
        ),
        Stage::Reverted | Stage::Successful => help_revert_and_transition(
            shared_state,
            thread_index,
            thread_and_sequence,
            NUM_WORDS - 1,
            || Ok(()),
        ),
    }
}
