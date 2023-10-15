#[cfg(feature = "tracing")]
use tracing::instrument;

#[cfg(feature = "alloc")]
use crate::sync::Arc;

use crate::err::{Error, FatalError};
use crate::kcas::{KCasWord, State};
use crate::sync::Ordering;
use crate::types::{
    convert_thread_id_to_thread_index, convert_thread_index_to_thread_id, ThreadId, ThreadIndex,
};

#[derive(Debug)]
pub enum UnsafeStateWrapperError {
    AttemptedToInitializeWithInvalidSharedState,
    NoThreadIdsAvailable,
}

#[derive(Debug)]
pub struct UnsafeStateWrapper<const NUM_THREADS: usize, const NUM_WORDS: usize> {
    shared_state: *const State<NUM_THREADS, NUM_WORDS>,
    thread_id: usize,
}

impl<const NUM_THREADS: usize, const NUM_WORDS: usize> UnsafeStateWrapper<NUM_THREADS, NUM_WORDS> {
    pub fn construct(
        shared_state: *const State<NUM_THREADS, NUM_WORDS>,
    ) -> Result<Self, UnsafeStateWrapperError> {
        let shared_state_ref = unsafe { shared_state.as_ref() }
            .ok_or(UnsafeStateWrapperError::AttemptedToInitializeWithInvalidSharedState)?;
        let thread_index: ThreadIndex = 'a: {
            for i in 0..NUM_THREADS {
                if shared_state_ref.thread_index_slots[i]
                    .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
                {
                    shared_state_ref
                        .num_threads_in_use
                        .fetch_add(1, Ordering::AcqRel);
                    break 'a i;
                }
            }
            return Err(UnsafeStateWrapperError::NoThreadIdsAvailable);
        };
        Ok(Self {
            shared_state,
            thread_id: convert_thread_index_to_thread_id(thread_index),
        })
    }

    #[cfg_attr(feature = "tracing", instrument)]
    pub fn kcas(&mut self, kcas_words: [KCasWord; NUM_WORDS]) -> Result<(), Error> {
        let shared_state: &State<NUM_THREADS, NUM_WORDS> =
            unsafe { self.shared_state.as_ref() }
                .ok_or(Error::Fatal(FatalError::SharedStateWasNotValidPointer))?;
        crate::kcas::kcas(shared_state, self.thread_id, kcas_words)
    }
}

impl<const NUM_THREADS: usize, const NUM_WORDS: usize> Drop
    for UnsafeStateWrapper<NUM_THREADS, NUM_WORDS>
{
    fn drop(&mut self) {
        let thread_index: ThreadIndex = convert_thread_id_to_thread_index(self.thread_id);

        // #[cfg(test)]
        // #[cfg(feature = "std")]
        // println!(
        //     "Dropping for thread with id: {} and index: {}",
        //     self.thread_id, thread_index
        // );

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

#[derive(Debug)]
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
                        .fetch_add(1, Ordering::AcqRel);
                    break 'a i;
                }
            }
            return Err(StateError::NoThreadIdsAvailable);
        };
        Ok(Self {
            shared_state,
            thread_id: convert_thread_index_to_thread_id(thread_index),
        })
    }

    #[cfg_attr(feature = "tracing", instrument)]
    pub fn kcas(&mut self, kcas_words: [KCasWord; NUM_WORDS]) -> Result<(), Error> {
        let shared_state: &State<NUM_THREADS, NUM_WORDS> = self.shared_state.as_ref();
        crate::kcas::kcas(shared_state, self.thread_id, kcas_words)
    }
}

#[cfg(feature = "alloc")]
impl<const NUM_THREADS: usize, const NUM_WORDS: usize> Drop
    for StateWrapper<NUM_THREADS, NUM_WORDS>
{
    fn drop(&mut self) {
        let thread_index: ThreadIndex = convert_thread_id_to_thread_index(self.thread_id);

        // #[cfg(test)]
        // #[cfg(feature = "std")]
        // println!(
        //     "Dropping for thread with id: {} and index: {}",
        //     self.thread_id, thread_index
        // );

        let shared_state_ref: &State<NUM_THREADS, NUM_WORDS> = self.shared_state.as_ref();
        shared_state_ref.thread_index_slots[thread_index].store(false, Ordering::Release);
        shared_state_ref
            .num_threads_in_use
            .fetch_min(1, Ordering::AcqRel);
    }
}

#[cfg(test)]
#[cfg(all(feature = "std", not(loom)))]
mod tests {
    use std::thread;
    use tracing::debug;
    use test_log::test;
    use crate::kcas::{KCasWord, State};
    use crate::sync::{Arc, AtomicUsize, Ordering};
    use crate::wrapper::{StateWrapper, UnsafeStateWrapper};
    use crate::err::Error;

    #[test]
    fn test_unsafe_wrapper() {
        let state: State<2, 3> = State::new();
        debug!("State after initialization: {state:?}");
        let state_pointer: *const State<2, 3> = &state;
        let mut first_unsafe_wrapper: UnsafeStateWrapper<2, 3> =
            UnsafeStateWrapper::construct(state_pointer).unwrap();
        let mut second_unsafe_wrapper: UnsafeStateWrapper<2, 3> =
            UnsafeStateWrapper::construct(state_pointer).unwrap();

        assert_eq!(first_unsafe_wrapper.thread_id, 1usize);
        assert_eq!(second_unsafe_wrapper.thread_id, 2usize);

        let first_location: AtomicUsize = AtomicUsize::new(50);
        let second_location: AtomicUsize = AtomicUsize::new(70);
        let third_location: AtomicUsize = AtomicUsize::new(90);
        debug!("first_location before kcas: {first_location:?}");
        debug!("second_location before kcas: {second_location:?}");
        debug!("third_location before kcas: {third_location:?}");

        first_unsafe_wrapper.kcas([
            KCasWord::new(&first_location, 50, 51),
            KCasWord::new(&second_location, 70, 71),
            KCasWord::new(&third_location, 90, 91),
        ]).unwrap();

        debug!("first_location before kcas: {first_location:?}");
        debug!("second_location before kcas: {second_location:?}");
        debug!("third_location before kcas: {third_location:?}");

        assert_eq!(first_location.load(Ordering::Acquire), 51);
        assert_eq!(second_location.load(Ordering::Acquire), 71);
        assert_eq!(third_location.load(Ordering::Acquire), 91);

        second_unsafe_wrapper.kcas([
            KCasWord::new(&first_location, 51, 52),
            KCasWord::new(&second_location, 71, 72),
            KCasWord::new(&third_location, 91, 92),
        ]).unwrap();

        assert_eq!(first_location.load(Ordering::Acquire), 52);
        assert_eq!(second_location.load(Ordering::Acquire), 72);
        assert_eq!(third_location.load(Ordering::Acquire), 92);
    }

    #[test]
    fn test_safe_wrapper() {
        let state: State<2, 3> = State::new();
        println!("State after initialization: {state:?}");
        let state_arc: Arc<State<2, 3>> = Arc::new(state);

        let mut first_state_wrapper: StateWrapper<2, 3> =
            StateWrapper::construct(state_arc.clone()).unwrap();
        let mut second_state_wrapper: StateWrapper<2, 3> =
            StateWrapper::construct(state_arc).unwrap();

        assert_eq!(first_state_wrapper.thread_id, 1usize);
        assert_eq!(second_state_wrapper.thread_id, 2usize);

        let first_location: AtomicUsize = AtomicUsize::new(50);
        let second_location: AtomicUsize = AtomicUsize::new(70);
        let third_location: AtomicUsize = AtomicUsize::new(90);
        debug!("first_location before kcas: {first_location:?}");
        debug!("second_location before kcas: {second_location:?}");
        debug!("third_location before kcas: {third_location:?}");

        first_state_wrapper.kcas([
            KCasWord::new(&first_location, 50, 51),
            KCasWord::new(&second_location, 70, 71),
            KCasWord::new(&third_location, 90, 91),
        ]).unwrap();

        debug!("first_location before kcas: {first_location:?}");
        debug!("second_location before kcas: {second_location:?}");
        debug!("third_location before kcas: {third_location:?}");

        assert_eq!(first_location.load(Ordering::Acquire), 51);
        assert_eq!(second_location.load(Ordering::Acquire), 71);
        assert_eq!(third_location.load(Ordering::Acquire), 91);

        second_state_wrapper.kcas([
            KCasWord::new(&first_location, 51, 52),
            KCasWord::new(&second_location, 71, 72),
            KCasWord::new(&third_location, 91, 92),
        ]).unwrap();

        assert_eq!(first_location.load(Ordering::Acquire), 52);
        assert_eq!(second_location.load(Ordering::Acquire), 72);
        assert_eq!(third_location.load(Ordering::Acquire), 92);
    }

    #[test]
    fn test_safe_concurrency() {
        let state: State<2, 3> = State::new();
        println!("State after initialization: {state:?}");
        let state_arc: Arc<State<2, 3>> = Arc::new(state);

        let mut first_state_wrapper: StateWrapper<2, 3> =
            StateWrapper::construct(state_arc.clone()).unwrap();
        let mut second_state_wrapper: StateWrapper<2, 3> =
            StateWrapper::construct(state_arc).unwrap();

        let first_location: Arc<AtomicUsize> = Arc::new(AtomicUsize::new(50));
        let second_location: Arc<AtomicUsize> = Arc::new(AtomicUsize::new(70));
        let third_location: Arc<AtomicUsize> = Arc::new(AtomicUsize::new(90));

        let first_location_clone: Arc<AtomicUsize> = first_location.clone();
        let second_location_clone: Arc<AtomicUsize> = second_location.clone();
        let third_location_clone: Arc<AtomicUsize> = third_location.clone();

        let first_location_clone_2: Arc<AtomicUsize> = first_location.clone();
        let second_location_clone_2: Arc<AtomicUsize> = second_location.clone();
        let third_location_clone_2: Arc<AtomicUsize> = third_location.clone();

        let first_handle: thread::JoinHandle<Result<(), Error>> = thread::spawn(move || {
            first_state_wrapper.kcas([
                KCasWord::new(first_location_clone.as_ref(), 50, 51),
                KCasWord::new(second_location_clone.as_ref(), 70, 71),
                KCasWord::new(third_location_clone.as_ref(), 90, 91),
            ])
        });

        let second_handle: thread::JoinHandle<Result<(), Error>> = thread::spawn(move || {
            second_state_wrapper.kcas([
                KCasWord::new(first_location_clone_2.as_ref(), 50, 52),
                KCasWord::new(second_location_clone_2.as_ref(), 70, 72),
                KCasWord::new(third_location_clone_2.as_ref(), 90, 92),
            ])
        });
        let second_result: Result<(), Error> = second_handle.join().expect("The second thread panicked");
        let first_result: Result<(), Error> = first_handle.join().expect("The first thread panicked");

        debug!("first_result: {:?}", first_result);
        debug!("second_result: {:?}", second_result);

        debug!("first_location after kcas: {first_location:?}");
        debug!("second_location after kcas: {second_location:?}");
        debug!("third_location after kcas: {third_location:?}");

        assert!(
            (matches!(first_result, Ok(_)) && matches!(second_result, Err(Error::ValueWasNotExpectedValue)))
            || (matches!(second_result, Ok(_)) && matches!(first_result, Err(Error::ValueWasNotExpectedValue)))
        );
        assert!(
            (
                first_location.load(Ordering::Acquire) ==  51
                && second_location.load(Ordering::Acquire) == 71
                && third_location.load(Ordering::Acquire) == 91
            )
                || (
                first_location.load(Ordering::Acquire) ==  52
                && second_location.load(Ordering::Acquire) == 72
                && third_location.load(Ordering::Acquire) == 92
            )
        );
        assert!(
            first_location.load(Ordering::Acquire) ==  51
            && second_location.load(Ordering::Acquire) == 71
            && third_location.load(Ordering::Acquire) == 91
        )
    }
}

#[cfg(test)]
#[cfg(feature = "std")]
#[cfg(loom)]
mod loom_tests {
    use loom::thread;
    use tracing::{debug, trace};
    use test_log::test;
    use crate::err::Error;
    use crate::kcas::{State, KCasWord};
    use crate::wrapper::StateWrapper;
    use crate::sync::{Arc, AtomicUsize, Ordering};

    #[test]
    fn loom_test() {
        loom::model(|| {
            let state: State<2, 3> = State::new();
            let state_arc: Arc<State<2, 3>> = Arc::new(state);

            let mut first_state_wrapper: StateWrapper<2, 3> =
                StateWrapper::construct(state_arc.clone()).unwrap();
            let mut second_state_wrapper: StateWrapper<2, 3> =
                StateWrapper::construct(state_arc).unwrap();

            let first_location: Arc<AtomicUsize> = Arc::new(AtomicUsize::new(50));
            let second_location: Arc<AtomicUsize> = Arc::new(AtomicUsize::new(70));
            let third_location: Arc<AtomicUsize> = Arc::new(AtomicUsize::new(90));

            let first_location_clone: Arc<AtomicUsize> = first_location.clone();
            let second_location_clone: Arc<AtomicUsize> = second_location.clone();
            let third_location_clone: Arc<AtomicUsize> = third_location.clone();

            let first_handle: thread::JoinHandle<Result<(), Error>> = thread::spawn(move || {
                first_state_wrapper.kcas([
                    KCasWord::new(first_location_clone.as_ref(), 50, 51),
                    KCasWord::new(second_location_clone.as_ref(), 70, 71),
                    KCasWord::new(third_location_clone.as_ref(), 90, 91),
                ])
            });

            let first_location_clone: Arc<AtomicUsize> = first_location.clone();
            let second_location_clone: Arc<AtomicUsize> = second_location.clone();
            let third_location_clone: Arc<AtomicUsize> = third_location.clone();

            let second_handle: thread::JoinHandle<Result<(), Error>> = thread::spawn(move || {
                second_state_wrapper.kcas([
                    KCasWord::new(first_location_clone.as_ref(), 50, 52),
                    KCasWord::new(second_location_clone.as_ref(), 70, 72),
                    KCasWord::new(third_location_clone.as_ref(), 90, 92),
                ])
            });
            let first_result: Result<(), Error> = first_handle.join().expect("The first thread panicked");
            let second_result: Result<(), Error> = second_handle.join().expect("The second thread panicked");

            trace!("first_result: {first_result:?}");
            debug!("second_result: {second_result:?}");
            assert!(matches!(first_result, Ok(_)) || matches!(first_result, Err(Error::ValueWasNotExpectedValue)));
            assert!(matches!(second_result, Ok(_)) || matches!(second_result, Err(Error::ValueWasNotExpectedValue)));
            assert!(
                (
                    first_location.load(Ordering::Acquire) ==  51
                    && second_location.load(Ordering::Acquire) == 71
                    && third_location.load(Ordering::Acquire) == 91
                )
                || (
                    first_location.load(Ordering::Acquire) ==  52
                    && second_location.load(Ordering::Acquire) == 72
                    && third_location.load(Ordering::Acquire) == 92
                )
            );
        })
    }
}

#[cfg(all(test, feature = "std", feature = "shuttle"))]
mod shuttle_tests {
    use shuttle::thread;
    use tracing::{debug, trace};
    use test_log::test;
    use crate::err::Error;
    use crate::kcas::{State, KCasWord};
    use crate::wrapper::StateWrapper;
    use crate::sync::{Arc, AtomicUsize, Ordering};

    #[test]
    fn shuttle_test() {
        shuttle::check_dfs(|| {
            let state: State<2, 3> = State::new();
            let state_arc: Arc<State<2, 3>> = Arc::new(state);

            let mut first_state_wrapper: StateWrapper<2, 3> =
                StateWrapper::construct(state_arc.clone()).unwrap();
            let mut second_state_wrapper: StateWrapper<2, 3> =
                StateWrapper::construct(state_arc).unwrap();

            let first_location: Arc<AtomicUsize> = Arc::new(AtomicUsize::new(50));
            let second_location: Arc<AtomicUsize> = Arc::new(AtomicUsize::new(70));
            let third_location: Arc<AtomicUsize> = Arc::new(AtomicUsize::new(90));

            let first_location_clone: Arc<AtomicUsize> = first_location.clone();
            let second_location_clone: Arc<AtomicUsize> = second_location.clone();
            let third_location_clone: Arc<AtomicUsize> = third_location.clone();

            let first_handle: thread::JoinHandle<Result<(), Error>> = thread::spawn(move || {
                first_state_wrapper.kcas([
                    KCasWord::new(first_location_clone.as_ref(), 50, 51),
                    KCasWord::new(second_location_clone.as_ref(), 70, 71),
                    KCasWord::new(third_location_clone.as_ref(), 90, 91),
                ])
            });

            let first_location_clone: Arc<AtomicUsize> = first_location.clone();
            let second_location_clone: Arc<AtomicUsize> = second_location.clone();
            let third_location_clone: Arc<AtomicUsize> = third_location.clone();

            let second_handle: thread::JoinHandle<Result<(), Error>> = thread::spawn(move || {
                second_state_wrapper.kcas([
                    KCasWord::new(first_location_clone.as_ref(), 50, 52),
                    KCasWord::new(second_location_clone.as_ref(), 70, 72),
                    KCasWord::new(third_location_clone.as_ref(), 90, 92),
                ])
            });
            let first_result: Result<(), Error> = first_handle.join().expect("The first thread panicked");
            let second_result: Result<(), Error> = second_handle.join().expect("The second thread panicked");

            trace!("first_result: {first_result:?}");
            debug!("second_result: {second_result:?}");
            assert!(matches!(first_result, Ok(_)) || matches!(first_result, Err(Error::ValueWasNotExpectedValue)));
            assert!(matches!(second_result, Ok(_)) || matches!(second_result, Err(Error::ValueWasNotExpectedValue)));
            assert!(
                (
                    first_location.load(Ordering::Acquire) ==  51
                        && second_location.load(Ordering::Acquire) == 71
                        && third_location.load(Ordering::Acquire) == 91
                )
                    || (
                    first_location.load(Ordering::Acquire) ==  52
                        && second_location.load(Ordering::Acquire) == 72
                        && third_location.load(Ordering::Acquire) == 92
                )
            );
        }, None);
    }
}