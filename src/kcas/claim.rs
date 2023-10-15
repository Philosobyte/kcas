use crate::err::{Error, FatalError};
use crate::kcas::revert::{help_revert_and_transition, revert_and_transition};
use crate::kcas::set::{help_set_and_transition, set_and_transition};
use crate::kcas::stage_change::{
    change_stage_and_transition,
    handle_concurrent_stage_and_sequence_verification_error_and_transition,
    help_change_stage_and_transition,
    help_handle_concurrent_stage_and_sequence_verification_error_and_transition,
    verify_stage_and_sequence_have_not_changed, StageAndSequenceVerificationError,
};
use crate::kcas::{
    extract_thread_from_thread_and_sequence, help_thread, prepare_for_next_operation, HelpError,
    State,
};
use crate::sync::{AtomicPtr, AtomicUsize, Ordering};
use crate::types::{
    get_bit_length_of_num_threads, SequenceNum, Stage, ThreadAndSequence, ThreadIndex, WordNum,
};
use displaydoc::Display;

#[cfg(feature = "tracing")]
use tracing::instrument;

/// Claim target addresses by swapping out expected values for [ThreadAndSequence] markers. Then,
/// move on to the [enum@Stage::Setting] stage if all values were expected; otherwise, move on to
/// [enum@Stage::Reverting].
#[cfg_attr(feature = "tracing", instrument)]
pub(super) fn claim_and_transition<const NUM_THREADS: usize, const NUM_WORDS: usize>(
    shared_state: &State<NUM_THREADS, NUM_WORDS>,
    thread_index: ThreadIndex,
    sequence: SequenceNum,
    thread_and_sequence: ThreadAndSequence,
) -> Result<(), Error> {
    let claim_result: Result<(), ClaimError> =
        claim(shared_state, thread_index, sequence, thread_and_sequence);
    if claim_result.is_ok() {
        // change the stage to Setting
        return change_stage_and_transition(
            shared_state,
            thread_index,
            sequence,
            thread_and_sequence,
            Stage::Claiming,
            Stage::Setting,
            // and then after that, try to set
            || set_and_transition(shared_state, thread_index, sequence, thread_and_sequence),
        );
    }
    match claim_result.unwrap_err() {
        ClaimError::StageAndSequenceVerificationError { error, .. } => {
            handle_concurrent_stage_and_sequence_verification_error_and_transition(
                shared_state,
                thread_index,
                sequence,
                thread_and_sequence,
                Stage::Claiming,
                error,
            )
        }
        // change the stage to Reverting first
        ClaimError::ValueWasNotExpectedValue { word_num, .. } => change_stage_and_transition(
            shared_state,
            thread_index,
            sequence,
            thread_and_sequence,
            Stage::Claiming,
            Stage::Reverting,
            // then revert
            ||  revert_and_transition(
                shared_state,
                thread_index,
                thread_and_sequence,
                word_num,
                || {
                    // then skip setting the stage to Reverted and just reset state
                    prepare_for_next_operation(shared_state, thread_index, sequence);
                    Err(Error::ValueWasNotExpectedValue)
                },
            ),
        ),
        ClaimError::TargetAddressWasNotValidPointer {
            word_num,
            target_address,
        } => Err(Error::Fatal(FatalError::TargetAddressWasNotValidPointer {
            word_num,
            target_address,
        })),
        ClaimError::FatalErrorWhileHelping(fatal_error) => Err(Error::Fatal(fatal_error)),
        ClaimError::TopBitsOfValueWereIllegal {
            word_num,
            target_address,
            value,
            num_reserved_bits,
        } => Err(Error::Fatal(FatalError::TopBitsOfValueWereIllegal {
            word_num,
            target_address,
            value,
            num_reserved_bits,
        })),
    }
}

/// Help another thread claim target addresses by swapping out expected values for
/// [ThreadAndSequence] markers. Then, move on to the [enum@Stage::Setting] stage if all values were
/// expected; otherwise, move on to [enum@Stage::Reverting].
#[cfg_attr(feature = "tracing", instrument)]
pub(super) fn help_claim_and_transition<const NUM_THREADS: usize, const NUM_WORDS: usize>(
    shared_state: &State<NUM_THREADS, NUM_WORDS>,
    thread_index: ThreadIndex,
    sequence: SequenceNum,
    thread_and_sequence: ThreadAndSequence,
) -> Result<(), HelpError> {
    let claim_result: Result<(), ClaimError> =
        claim(shared_state, thread_index, sequence, thread_and_sequence);
    if claim_result.is_ok() {
        return help_change_stage_and_transition(
            shared_state,
            thread_index,
            sequence,
            thread_and_sequence,
            Stage::Claiming,
            Stage::Setting,
            || help_set_and_transition(shared_state, thread_index, sequence, thread_and_sequence),
        );
    }
    match claim_result.unwrap_err() {
        ClaimError::StageAndSequenceVerificationError {
            error,
            ..
        } => help_handle_concurrent_stage_and_sequence_verification_error_and_transition(
            shared_state,
            thread_index,
            sequence,
            thread_and_sequence,
            Stage::Claiming,
            error,
        ),
        // change the stage to Reverting first
        ClaimError::ValueWasNotExpectedValue { word_num, .. } => help_change_stage_and_transition(
            shared_state,
            thread_index,
            sequence,
            thread_and_sequence,
            Stage::Claiming,
            Stage::Reverting,
            // then revert
            || {
                help_revert_and_transition(
                    shared_state,
                    thread_index,
                    thread_and_sequence,
                    word_num,
                    // then change the stage to Reverted
                    || help_change_stage_and_transition(
                        shared_state,
                        thread_index,
                        sequence,
                        thread_and_sequence,
                        Stage::Reverting,
                        Stage::Reverted,
                        || Ok(()),
                    ),
                )
            },
        ),
        ClaimError::TargetAddressWasNotValidPointer {
            word_num,
            target_address,
        } => Err(HelpError::Fatal(
            FatalError::TargetAddressWasNotValidPointer {
                word_num,
                target_address,
            },
        )),
        ClaimError::FatalErrorWhileHelping(fatal_error) => Err(HelpError::Fatal(fatal_error)),
        ClaimError::TopBitsOfValueWereIllegal {
            word_num,
            target_address,
            value,
            num_reserved_bits,
        } => Err(HelpError::Fatal(FatalError::TopBitsOfValueWereIllegal {
            word_num,
            target_address,
            value,
            num_reserved_bits,
        })),
    }
}

#[derive(Debug, Display)]
enum ClaimError {
    /** The stage or sequence changed while attempting to claim the target address at word
        {failed_word_num}: {error}
     */
    StageAndSequenceVerificationError {
        failed_word_num: WordNum,
        error: StageAndSequenceVerificationError,
    },
    /** The value at word number {word_num} and target address {target_address} was not the
        expected value but was instead: {actual_value}
     */
    ValueWasNotExpectedValue {
        word_num: WordNum,
        target_address: usize,
        actual_value: usize,
    },
    /// The target address at {target_address} for word number {word_num} was not a valid pointer.
    TargetAddressWasNotValidPointer {
        word_num: WordNum,
        target_address: usize,
    },
    /// Encountered a fatal error while helping another thread: {0}
    FatalErrorWhileHelping(FatalError),
    /** Encountered unexpected values for the top {num_reserved_bits} bits of the value {value} at
        target address {target_address} for word number {word_num}.
    */
    TopBitsOfValueWereIllegal {
        word_num: WordNum,
        target_address: usize,
        value: usize,
        num_reserved_bits: usize,
    },
}

/// Claim target addresses by swapping out expected values for [ThreadAndSequence] markers.
#[cfg_attr(feature = "tracing", instrument)]
fn claim<const NUM_THREADS: usize, const NUM_WORDS: usize>(
    state: &State<NUM_THREADS, NUM_WORDS>,
    thread_index: ThreadIndex,
    sequence: SequenceNum,
    thread_and_sequence: ThreadAndSequence,
) -> Result<(), ClaimError> {
    for word_num in 0..NUM_WORDS {
        let target_address_ptr: &AtomicPtr<AtomicUsize> =
            &state.target_addresses[thread_index][word_num];
        let target_address_ptr: *mut AtomicUsize = target_address_ptr.load(Ordering::Acquire);

        let target_address: &AtomicUsize = unsafe { target_address_ptr.as_ref() }
            .ok_or(ClaimError::TargetAddressWasNotValidPointer {
                word_num,
                target_address: target_address_ptr as usize,
            })?;

        let expected_element: usize =
            state.expected_values[thread_index][word_num].load(Ordering::Acquire);

        'cas_loop: while let Err(actual_value) = target_address.compare_exchange_weak(
            expected_element,
            thread_and_sequence,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            if actual_value == expected_element {
                continue 'cas_loop;
            }
            if actual_value == thread_and_sequence {
                // value is already the marker we wanted
                break 'cas_loop;
            }

            if let Err(error) = verify_stage_and_sequence_have_not_changed(
                state,
                thread_index,
                Stage::Claiming,
                sequence,
            ) {
                return Err(ClaimError::StageAndSequenceVerificationError {
                    failed_word_num: word_num,
                    error,
                });
            }

            // in case the value is a pointer, canonical pointers typically set all the bits
            // above virtual address space to either 0 or 1
            let bit_length_of_num_threads: usize =
                get_bit_length_of_num_threads::<NUM_THREADS>();
            let num_sequence_bits: usize = usize::BITS as usize - bit_length_of_num_threads;
            let top_bits_if_value_is_kernel_pointer: usize = usize::MAX >> num_sequence_bits;

            let top_bits: usize = extract_thread_from_thread_and_sequence::<NUM_THREADS>(actual_value);

            if top_bits == 0 || top_bits == top_bits_if_value_is_kernel_pointer {
                // happy path for a CAS failure
                return Err(ClaimError::ValueWasNotExpectedValue {
                    word_num,
                    target_address: target_address_ptr as usize,
                    actual_value,
                });
            } else if top_bits <= NUM_THREADS {
                // the top bits were a ThreadId
                let help_result: Result<(), HelpError> = help_thread(state, actual_value);
                if help_result.is_ok() {
                    // The other thread's ThreadAndSequence should be gone. Try again.
                    continue 'cas_loop;
                }
                let help_error: HelpError = help_result.unwrap_err();
                match help_error {
                    HelpError::SequenceChangedWhileHelping { .. } | HelpError::HelpeeStageIsAlreadyTerminal(..) => {
                        // Either of these means the other thread's ThreadAndSequence should be
                        // gone. Try again.
                        continue 'cas_loop;
                    }
                    HelpError::Fatal(fatal_error) => {
                        return Err(ClaimError::FatalErrorWhileHelping(fatal_error));
                    }
                }
            } else {
                return Err(ClaimError::TopBitsOfValueWereIllegal {
                    word_num,
                    target_address: target_address_ptr as usize,
                    value: actual_value,
                    num_reserved_bits: bit_length_of_num_threads,
                });
            }
        }
    }
    Ok(())
}
