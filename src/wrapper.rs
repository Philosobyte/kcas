#[cfg(feature = "alloc")]
use alloc::sync::Arc;

use crate::aliases::{thread_id_to_thread_index, thread_index_to_thread_id, ThreadIndex};
use crate::kcas::State;
use core::sync::atomic::Ordering;

#[derive(Debug)]
pub enum UnsafeStateWrapperError {
    AttemptedToInitializeWithInvalidSharedState,
    NoThreadIdsAvailable,
}

#[derive(Debug)]
pub struct UnsafeStateWrapper<const NUM_THREADS: usize, const NUM_WORDS: usize> {
    shared_state: *mut State<NUM_THREADS, NUM_WORDS>,
    thread_id: usize,
}

impl<const NUM_THREADS: usize, const NUM_WORDS: usize> UnsafeStateWrapper<NUM_THREADS, NUM_WORDS> {
    pub fn construct(
        shared_state: *mut State<NUM_THREADS, NUM_WORDS>,
    ) -> Result<Self, UnsafeStateWrapperError> {
        let shared_state_ref = unsafe { shared_state.as_mut() }
            .ok_or(UnsafeStateWrapperError::AttemptedToInitializeWithInvalidSharedState)?;
        let thread_index: ThreadIndex = 'a: {
            for i in 0..NUM_THREADS {
                if shared_state_ref.thread_index_slots[i]
                    .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
                {
                    shared_state_ref
                        .num_threads_in_use
                        .fetch_add(1, Ordering::Acquire);
                    break 'a i;
                }
            }
            return Err(UnsafeStateWrapperError::NoThreadIdsAvailable);
        };
        Ok(Self {
            shared_state,
            thread_id: thread_index_to_thread_id(thread_index),
        })
    }
}

impl<const NUM_THREADS: usize, const NUM_WORDS: usize> Drop
    for UnsafeStateWrapper<NUM_THREADS, NUM_WORDS>
{
    fn drop(&mut self) {
        let thread_index: ThreadIndex = thread_id_to_thread_index(self.thread_id);

        #[cfg(test)]
        #[cfg(feature = "std")]
        println!(
            "Dropping for thread with id: {} and index: {}",
            self.thread_id, thread_index
        );

        let shared_state_ref = match unsafe { self.shared_state.as_ref() } {
            None => return,
            Some(shared_state) => shared_state,
        };
        shared_state_ref.thread_index_slots[thread_index].store(false, Ordering::Release);
        shared_state_ref
            .num_threads_in_use
            .fetch_min(1, Ordering::AcqRel);
    }
}

pub enum StateError {
    NoThreadIdsAvailable,
}

#[derive(Debug)]
#[cfg(feature = "alloc")]
pub struct StateWrapper<const NUM_THREADS: usize, const NUM_WORDS: usize> {
    shared_state: Arc<State<NUM_THREADS, NUM_WORDS>>,
    thread_id: usize,
}

#[cfg(feature = "alloc")]
impl<const NUM_THREADS: usize, const NUM_WORDS: usize> StateWrapper<NUM_THREADS, NUM_WORDS> {
    pub fn construct(shared_state: Arc<State<NUM_THREADS, NUM_WORDS>>) -> Result<Self, StateError> {
        let shared_state_ref: &State<NUM_THREADS, NUM_WORDS> = shared_state.as_ref();

        let thread_index: ThreadIndex = 'a: {
            for i in 0..NUM_THREADS {
                if shared_state_ref.thread_index_slots[i]
                    .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
                {
                    shared_state_ref
                        .num_threads_in_use
                        .fetch_add(1, Ordering::Acquire);
                    break 'a i;
                }
            }
            return Err(StateError::NoThreadIdsAvailable);
        };
        Ok(Self {
            shared_state,
            thread_id: thread_index_to_thread_id(thread_index),
        })
    }
}

#[cfg(feature = "alloc")]
impl<const NUM_THREADS: usize, const NUM_WORDS: usize> Drop
    for StateWrapper<NUM_THREADS, NUM_WORDS>
{
    fn drop(&mut self) {
        let thread_index: ThreadIndex = thread_id_to_thread_index(self.thread_id);

        #[cfg(test)]
        #[cfg(feature = "std")]
        println!(
            "Dropping for thread with id: {} and index: {}",
            self.thread_id, thread_index
        );

        let shared_state_ref: &State<NUM_THREADS, NUM_WORDS> = self.shared_state.as_ref();
        shared_state_ref.thread_index_slots[thread_index].store(false, Ordering::Release);
        shared_state_ref
            .num_threads_in_use
            .fetch_min(1, Ordering::AcqRel);
    }
}
