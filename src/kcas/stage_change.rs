use crate::aliases::{
    thread_index_to_thread_id, SequenceNum, StageAndSequence, ThreadAndSequence, ThreadIndex,
};
use crate::err::{FatalError, KCasError, StageOutOfBoundsError};
use crate::kcas::revert::{help_revert_and_transition, revert_and_transition};
use crate::kcas::set::{help_set_and_transition, set_and_transition};
use crate::kcas::{
    combine_stage_and_sequence, extract_sequence_from_stage_and_sequence,
    extract_stage_from_stage_and_sequence, reset_for_next_operation, HelpError, State,
};
use crate::stage::Stage;
use crate::sync::Ordering;

pub(super) enum StageAndSequenceVerificationError {
    StageChanged { actual_stage: Stage },
    SequenceChanged { actual_sequence: SequenceNum },
    StageOutOfBounds { actual_stage_number: usize },
}

impl From<StageOutOfBoundsError> for StageAndSequenceVerificationError {
    fn from(stage_out_of_bounds_error: StageOutOfBoundsError) -> Self {
        Self::StageOutOfBounds {
            actual_stage_number: stage_out_of_bounds_error.0,
        }
    }
}

pub(super) fn verify_stage_and_sequence_have_not_changed<
    const NUM_THREADS: usize,
    const NUM_WORDS: usize,
>(
    thread_state: &State<NUM_THREADS, NUM_WORDS>,
    thread_index: usize,
    expected_stage: Stage,
    expected_sequence: SequenceNum,
) -> Result<(), StageAndSequenceVerificationError> {
    let actual_stage_and_sequence: StageAndSequence =
        thread_state.stage_and_sequence_numbers[thread_index].load(Ordering::Acquire);
    let actual_sequence: SequenceNum =
        extract_sequence_from_stage_and_sequence(actual_stage_and_sequence);
    let actual_stage: Stage = extract_stage_from_stage_and_sequence(actual_stage_and_sequence)?;

    if actual_sequence != expected_sequence {
        return Err(StageAndSequenceVerificationError::SequenceChanged { actual_sequence });
    }
    if actual_stage != expected_stage {
        return Err(StageAndSequenceVerificationError::StageChanged { actual_stage });
    }
    Ok(())
}

pub(super) fn change_stage_and_transition<const NUM_THREADS: usize, const NUM_WORDS: usize, F>(
    shared_state: &State<NUM_THREADS, NUM_WORDS>,
    thread_index: ThreadIndex,
    sequence: SequenceNum,
    thread_and_sequence: ThreadAndSequence,
    current_stage_and_sequence: StageAndSequence,
    current_stage: Stage,
    desired_stage: Stage,
    next_function: F,
) -> Result<(), KCasError>
where
    F: FnOnce() -> Result<(), KCasError>,
{
    let desired_stage_and_sequence: StageAndSequence =
        combine_stage_and_sequence(desired_stage, sequence);

    let cas_result: Result<usize, usize> = shared_state.stage_and_sequence_numbers[thread_index]
        .compare_exchange(
            current_stage_and_sequence,
            desired_stage_and_sequence,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    if cas_result.is_ok() {
        return next_function();
    }
    let actual_stage_and_sequence: StageAndSequence = cas_result.unwrap_err();
    let actual_stage: Stage = extract_stage_from_stage_and_sequence(actual_stage_and_sequence)
        .map_err(|e| FatalError::from(e))?;
    let actual_sequence: SequenceNum =
        extract_sequence_from_stage_and_sequence(actual_stage_and_sequence);

    if actual_sequence != sequence {
        return revert_and_transition(
            shared_state,
            thread_index,
            thread_and_sequence,
            NUM_WORDS - 1,
            || {
                Err(KCasError::Fatal(
                    FatalError::SequenceNumChangedByNonOriginalThread {
                        original_thread_id: thread_index_to_thread_id(thread_index),
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

pub(super) fn help_change_stage_and_transition<
    const NUM_THREADS: usize,
    const NUM_WORDS: usize,
    F,
>(
    shared_state: &State<NUM_THREADS, NUM_WORDS>,
    thread_index: ThreadIndex,
    sequence: SequenceNum,
    thread_and_sequence: ThreadAndSequence,
    current_stage_and_sequence: StageAndSequence,
    current_stage: Stage,
    desired_stage: Stage,
    next_function: F,
) -> Result<(), HelpError>
where
    F: FnOnce() -> Result<(), HelpError>,
{
    let desired_stage_and_sequence: StageAndSequence =
        combine_stage_and_sequence(desired_stage, sequence);

    let cas_result: Result<usize, usize> = shared_state.stage_and_sequence_numbers[thread_index]
        .compare_exchange(
            current_stage_and_sequence,
            desired_stage_and_sequence,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    if cas_result.is_ok() {
        return next_function();
    }
    let actual_stage_and_sequence: StageAndSequence = cas_result.unwrap_err();
    let actual_stage: Stage = extract_stage_from_stage_and_sequence(actual_stage_and_sequence)
        .map_err(|e| FatalError::from(e))?;
    let actual_sequence: SequenceNum =
        extract_sequence_from_stage_and_sequence(actual_stage_and_sequence);

    if actual_sequence != sequence {
        return help_revert_and_transition(
            shared_state,
            thread_index,
            thread_and_sequence,
            NUM_WORDS - 1,
            || Err(HelpError::SequenceChangedWhileHelping),
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
) -> Result<(), KCasError> {
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
        StageAndSequenceVerificationError::SequenceChanged { .. } => Err(KCasError::Fatal(
            FatalError::SequenceNumChangedByNonOriginalThread {
                original_thread_id: thread_index_to_thread_id(thread_index),
            },
        )),
        StageAndSequenceVerificationError::StageOutOfBounds {
            actual_stage_number,
        } => Err(KCasError::Fatal(FatalError::StageOutOfBounds(
            actual_stage_number,
        ))),
    }
}

pub(super) fn help_handle_concurrent_stage_and_sequence_verification_error_and_transition<
    const NUM_THREADS: usize,
    const NUM_WORDS: usize,
>(
    shared_state: &State<NUM_THREADS, NUM_WORDS>,
    thread_index: ThreadIndex,
    sequence: SequenceNum,
    thread_and_sequence: ThreadAndSequence,
    current_stage: Stage,
    error: StageAndSequenceVerificationError,
) -> Result<(), HelpError> {
    match error {
        StageAndSequenceVerificationError::StageChanged { actual_stage } => {
            help_handle_concurrent_stage_change_and_transition(
                shared_state,
                thread_index,
                sequence,
                thread_and_sequence,
                current_stage,
                actual_stage,
            )
        }
        StageAndSequenceVerificationError::SequenceChanged { .. } => {
            Err(HelpError::SequenceChangedWhileHelping)
        }
        StageAndSequenceVerificationError::StageOutOfBounds {
            actual_stage_number,
        } => Err(HelpError::Fatal(FatalError::StageOutOfBounds(
            actual_stage_number,
        ))),
    }
}

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
) -> Result<(), KCasError> {
    match actual_stage {
        // not possible - fatal - don't even bother reverting
        Stage::Inactive | Stage::Claiming => {
            Err(KCasError::Fatal(FatalError::IllegalStageChange {
                original_stage: current_stage,
                actual_stage,
            }))
        }
        Stage::Setting => {
            set_and_transition(shared_state, thread_index, sequence, thread_and_sequence)
        }
        Stage::Reverting | Stage::Reverted => revert_and_transition(
            shared_state,
            thread_index,
            thread_and_sequence,
            NUM_WORDS - 1,
            || {
                reset_for_next_operation(shared_state, thread_index, sequence);
                Err(KCasError::ValueWasNotExpectedValue)
            },
        ),
        Stage::Successful => revert_and_transition(
            shared_state,
            thread_index,
            thread_and_sequence,
            NUM_WORDS - 1,
            || {
                reset_for_next_operation(shared_state, thread_index, sequence);
                Ok(())
            },
        ),
    }
}

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
                    combine_stage_and_sequence(Stage::Reverting, sequence),
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
