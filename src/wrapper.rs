use core::ptr::NonNull;
use displaydoc::Display;

use tracing::instrument;

#[cfg(any(feature = "std", feature = "alloc"))]
use crate::sync::Arc;

use crate::err::Error;
use crate::kcas::{KCasWord, State};
use crate::sync::Ordering;
use crate::types::{
    convert_thread_id_to_thread_index, convert_thread_index_to_thread_id, ThreadIndex,
};

/// All thread slots for this [State] are already in use.
#[derive(Debug, Display)]
pub struct NoThreadIdAvailableError;

fn find_next_available_thread_index<const NUM_THREADS: usize, const NUM_WORDS: usize>(
    state: &State<NUM_THREADS, NUM_WORDS>,
) -> Result<ThreadIndex, NoThreadIdAvailableError> {
    for i in 0..NUM_THREADS {
        let cas_result: Result<bool, bool> = state.thread_index_slots[i].compare_exchange(
            false,
            true,
            Ordering::AcqRel,
            Ordering::Acquire,
        );

        if cas_result.is_ok() {
            state.num_threads_in_use.fetch_add(1, Ordering::AcqRel);
            return Ok(i);
        }
    }
    Err(NoThreadIdAvailableError)
}

#[derive(Debug, Display)]
pub enum ArcStateWrapperError {
    /** Could not construct [ArcStateWrapper] because all thread slots for the provided [State] are
       already in use.
    */
    NoThreadIdAvailable(NoThreadIdAvailableError),
}

impl From<NoThreadIdAvailableError> for ArcStateWrapperError {
    fn from(error: NoThreadIdAvailableError) -> Self {
        Self::NoThreadIdAvailable(error)
    }
}

#[derive(Debug)]
#[cfg(any(feature = "std", feature = "alloc"))]
pub struct ArcStateWrapper<const NUM_THREADS: usize, const NUM_WORDS: usize> {
    shared_state: Arc<State<NUM_THREADS, NUM_WORDS>>,
    thread_id: usize,
}

#[cfg(any(feature = "std", feature = "alloc"))]
impl<const NUM_THREADS: usize, const NUM_WORDS: usize> ArcStateWrapper<NUM_THREADS, NUM_WORDS> {
    pub fn construct(
        state: Arc<State<NUM_THREADS, NUM_WORDS>>,
    ) -> Result<Self, ArcStateWrapperError> {
        let state_ref: &State<NUM_THREADS, NUM_WORDS> = state.as_ref();

        let thread_index: ThreadIndex = find_next_available_thread_index(state_ref)?;
        Ok(Self {
            shared_state: state,
            thread_id: convert_thread_index_to_thread_id(thread_index),
        })
    }

    #[instrument]
    pub fn kcas(&mut self, kcas_words: [KCasWord; NUM_WORDS]) -> Result<(), Error> {
        let shared_state: &State<NUM_THREADS, NUM_WORDS> = self.shared_state.as_ref();
        crate::kcas::kcas(shared_state, self.thread_id, kcas_words)
    }
}

#[cfg(any(feature = "std", feature = "alloc"))]
impl<const NUM_THREADS: usize, const NUM_WORDS: usize> Drop
    for ArcStateWrapper<NUM_THREADS, NUM_WORDS>
{
    fn drop(&mut self) {
        let thread_index: ThreadIndex = convert_thread_id_to_thread_index(self.thread_id);

        let shared_state_ref: &State<NUM_THREADS, NUM_WORDS> = self.shared_state.as_ref();
        shared_state_ref.thread_index_slots[thread_index].store(false, Ordering::Release);
        shared_state_ref
            .num_threads_in_use
            .fetch_min(1, Ordering::AcqRel);
    }
}

#[derive(Debug, Display)]
pub enum RefStateWrapperError {
    /** Could not construct [RefStateWrapper] because all thread slots for the provided [State]
       are already in use.
    */
    NoThreadIdAvailable(NoThreadIdAvailableError),
}

impl From<NoThreadIdAvailableError> for RefStateWrapperError {
    fn from(error: NoThreadIdAvailableError) -> Self {
        Self::NoThreadIdAvailable(error)
    }
}

#[derive(Debug)]
pub struct RefStateWrapper<'a, const NUM_THREADS: usize, const NUM_WORDS: usize> {
    shared_state: &'a State<NUM_THREADS, NUM_WORDS>,
    thread_id: usize,
}

impl<'a, const NUM_THREADS: usize, const NUM_WORDS: usize>
    RefStateWrapper<'a, NUM_THREADS, NUM_WORDS>
{
    pub fn construct(
        shared_state: &'a State<NUM_THREADS, NUM_WORDS>,
    ) -> Result<Self, RefStateWrapperError> {
        let thread_index: ThreadIndex = find_next_available_thread_index(shared_state)?;
        Ok(Self {
            shared_state,
            thread_id: convert_thread_index_to_thread_id(thread_index),
        })
    }

    #[instrument]
    pub fn kcas(&mut self, kcas_words: [KCasWord; NUM_WORDS]) -> Result<(), Error> {
        crate::kcas::kcas(self.shared_state, self.thread_id, kcas_words)
    }
}

impl<'a, const NUM_THREADS: usize, const NUM_WORDS: usize> Drop
    for RefStateWrapper<'a, NUM_THREADS, NUM_WORDS>
{
    fn drop(&mut self) {
        let thread_index: ThreadIndex = convert_thread_id_to_thread_index(self.thread_id);

        self.shared_state.thread_index_slots[thread_index].store(false, Ordering::Release);
        self.shared_state
            .num_threads_in_use
            .fetch_min(1, Ordering::AcqRel);
    }
}

#[derive(Debug, Display)]
pub enum UnsafeStateWrapperError {
    /** Could not construct [UnsafeStateWrapper] because all thread slots for the provided [State]
          are already in use.
    */
    NoThreadIdAvailable(NoThreadIdAvailableError),
}

impl From<NoThreadIdAvailableError> for UnsafeStateWrapperError {
    fn from(error: NoThreadIdAvailableError) -> Self {
        Self::NoThreadIdAvailable(error)
    }
}

#[derive(Debug)]
pub struct UnsafeStateWrapper<const NUM_THREADS: usize, const NUM_WORDS: usize> {
    shared_state: NonNull<State<NUM_THREADS, NUM_WORDS>>,
    thread_id: usize,
}

impl<const NUM_THREADS: usize, const NUM_WORDS: usize> UnsafeStateWrapper<NUM_THREADS, NUM_WORDS> {
    pub fn construct(
        shared_state: NonNull<State<NUM_THREADS, NUM_WORDS>>,
    ) -> Result<Self, UnsafeStateWrapperError> {
        let shared_state_ref = unsafe { shared_state.as_ref() };
        let thread_index: ThreadIndex = find_next_available_thread_index(shared_state_ref)?;
        Ok(Self {
            shared_state,
            thread_id: convert_thread_index_to_thread_id(thread_index),
        })
    }

    #[instrument]
    pub fn kcas(&mut self, kcas_words: [KCasWord; NUM_WORDS]) -> Result<(), Error> {
        let shared_state: &State<NUM_THREADS, NUM_WORDS> = unsafe { self.shared_state.as_ref() };
        crate::kcas::kcas(shared_state, self.thread_id, kcas_words)
    }
}

impl<const NUM_THREADS: usize, const NUM_WORDS: usize> Drop
    for UnsafeStateWrapper<NUM_THREADS, NUM_WORDS>
{
    fn drop(&mut self) {
        let thread_index: ThreadIndex = convert_thread_id_to_thread_index(self.thread_id);

        let shared_state: &State<NUM_THREADS, NUM_WORDS> = unsafe { self.shared_state.as_ref() };
        shared_state.thread_index_slots[thread_index].store(false, Ordering::Release);
        shared_state
            .num_threads_in_use
            .fetch_min(1, Ordering::AcqRel);
    }
}

unsafe impl<const NUM_THREADS: usize, const NUM_WORDS: usize> Send for UnsafeStateWrapper<NUM_THREADS, NUM_WORDS> {}
unsafe impl<const NUM_THREADS: usize, const NUM_WORDS: usize> Sync for UnsafeStateWrapper<NUM_THREADS, NUM_WORDS> {}

#[cfg(all(test, feature = "std", not(feature = "shuttle"), not(loom)))]
mod tests {
    use crate::err::Error;
    use crate::kcas::{KCasWord, State};
    use crate::sync::{Arc, AtomicUsize, Ordering};
    use crate::wrapper::{
        ArcStateWrapper, ArcStateWrapperError, RefStateWrapper, RefStateWrapperError,
        UnsafeStateWrapper, UnsafeStateWrapperError,
    };
    use std::ptr::NonNull;
    use std::thread;
    use std::thread::ScopedJoinHandle;
    use test_log::test;

    #[test]
    fn test_arc_state_wrapper_thread_reservation() {
        let state: State<3, 3> = State::new();
        let state_arc: Arc<State<3, 3>> = Arc::new(state);

        let first_wrapper: ArcStateWrapper<3, 3> =
            ArcStateWrapper::construct(state_arc.clone()).unwrap();
        assert_eq!(first_wrapper.thread_id, 1);
        {
            let second_wrapper = ArcStateWrapper::construct(state_arc.clone()).unwrap();
            assert_eq!(second_wrapper.thread_id, 2);
        }
        // the second wrapper as been dropped - thread id 2 should be available again
        let second_wrapper: ArcStateWrapper<3, 3> =
            ArcStateWrapper::construct(state_arc.clone()).unwrap();
        assert_eq!(second_wrapper.thread_id, 2);

        let third_wrapper: ArcStateWrapper<3, 3> =
            ArcStateWrapper::construct(state_arc.clone()).unwrap();
        assert_eq!(third_wrapper.thread_id, 3);

        // now there should be no thread ids left
        let result: Result<ArcStateWrapper<3, 3>, ArcStateWrapperError> =
            ArcStateWrapper::construct(state_arc.clone());
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ArcStateWrapperError::NoThreadIdAvailable(_)
        ));
    }

    #[test]
    fn test_ref_state_wrapper_thread_reservation() {
        let state: State<3, 3> = State::new();

        let first_wrapper: RefStateWrapper<3, 3> = RefStateWrapper::construct(&state).unwrap();
        assert_eq!(first_wrapper.thread_id, 1);
        {
            let second_wrapper: RefStateWrapper<3, 3> = RefStateWrapper::construct(&state).unwrap();
            assert_eq!(second_wrapper.thread_id, 2);
        }
        // the second wrapper as been dropped - thread id 2 should be available again
        let second_wrapper: RefStateWrapper<3, 3> = RefStateWrapper::construct(&state).unwrap();
        assert_eq!(second_wrapper.thread_id, 2);

        let third_wrapper: RefStateWrapper<3, 3> = RefStateWrapper::construct(&state).unwrap();
        assert_eq!(third_wrapper.thread_id, 3);

        // now there should be no thread ids left
        let result: Result<RefStateWrapper<3, 3>, RefStateWrapperError> =
            RefStateWrapper::construct(&state);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            RefStateWrapperError::NoThreadIdAvailable(_)
        ));
    }

    #[test]
    fn test_unsafe_state_wrapper_thread_reservation() {
        let state: State<3, 3> = State::new();
        let non_null_state: NonNull<State<3, 3>> = NonNull::from(&state);

        let first_wrapper: UnsafeStateWrapper<3, 3> =
            UnsafeStateWrapper::construct(non_null_state).unwrap();
        assert_eq!(first_wrapper.thread_id, 1);
        {
            let second_wrapper: UnsafeStateWrapper<3, 3> =
                UnsafeStateWrapper::construct(non_null_state).unwrap();
            assert_eq!(second_wrapper.thread_id, 2);
        }
        // the second wrapper as been dropped - thread id 2 should be available again
        let second_wrapper: UnsafeStateWrapper<3, 3> =
            UnsafeStateWrapper::construct(non_null_state).unwrap();
        assert_eq!(second_wrapper.thread_id, 2);

        let third_wrapper: UnsafeStateWrapper<3, 3> =
            UnsafeStateWrapper::construct(non_null_state).unwrap();
        assert_eq!(third_wrapper.thread_id, 3);

        // now there should be no thread ids left
        let result: Result<UnsafeStateWrapper<3, 3>, UnsafeStateWrapperError> =
            UnsafeStateWrapper::construct(non_null_state);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            UnsafeStateWrapperError::NoThreadIdAvailable(_)
        ));
    }

    #[test]
    fn test_arc_state_wrapper_with_2_threads() {
        test_arc_state_wrapper::<2, 3>();
    }

    fn test_arc_state_wrapper<const NUM_THREADS: usize, const NUM_WORDS: usize>() {
        let state: State<NUM_THREADS, NUM_WORDS> = State::new();
        let state_arc: Arc<State<NUM_THREADS, NUM_WORDS>> = Arc::new(state);

        let targets: [Arc<AtomicUsize>; NUM_WORDS] =
            core::array::from_fn(|_| Arc::new(AtomicUsize::new(0)));

        let join_handles: Vec<thread::JoinHandle<Result<(), Error>>> = (0..NUM_THREADS)
            .map(|i| {
                let mut wrapper: ArcStateWrapper<NUM_THREADS, NUM_WORDS> =
                    ArcStateWrapper::construct(state_arc.clone()).unwrap();
                let cloned_targets: Vec<Arc<AtomicUsize>> = targets
                    .iter()
                    .cloned()
                    .collect();
                let handle: thread::JoinHandle<Result<(), Error>> = thread::spawn(move || {
                    let cloned_targets = cloned_targets;
                    let kcas_words: Vec<KCasWord> = cloned_targets
                        .iter()
                        .map(|target_arc| KCasWord::new(target_arc.as_ref(), 0, i))
                        .collect();
                    let kcas_words: [KCasWord; NUM_WORDS] = kcas_words.try_into().unwrap();
                    wrapper.kcas(kcas_words)
                });
                handle
            })
            .collect();

        join_handles.into_iter().for_each(|join_handle| {
            let result: Result<(), Error> = join_handle.join().expect("A thread panicked");
            assert!(matches!(result, Ok(_)) || matches!(result, Err(Error::ValueWasNotExpectedValue)));
        });
        assert!((0..NUM_THREADS).any(|i| targets
            .iter()
            .all(|target| target.load(Ordering::Acquire) == i)));
    }

    #[test]
    fn test_ref_state_wrapper_with_2_threads() {
        test_ref_state_wrapper::<2, 3>();
    }

    fn test_ref_state_wrapper<const NUM_THREADS: usize, const NUM_WORDS: usize>() {
        let state: State<NUM_THREADS, NUM_WORDS> = State::new();

        let targets: [Arc<AtomicUsize>; NUM_WORDS] =
            core::array::from_fn(|_| Arc::new(AtomicUsize::new(0)));

        thread::scope(|scope| {
            let join_handles: Vec<ScopedJoinHandle<Result<(), Error>>> = (0..NUM_THREADS)
                .map(|i| {
                    let mut wrapper: RefStateWrapper<NUM_THREADS, NUM_WORDS> =
                        RefStateWrapper::construct(&state).unwrap();
                    let cloned_targets: Vec<Arc<AtomicUsize>> = targets
                        .iter()
                        .cloned()
                        .collect();
                    let handle: ScopedJoinHandle<Result<(), Error>> = scope.spawn(move || {
                        let cloned_targets = cloned_targets;
                        let kcas_words: Vec<KCasWord> = cloned_targets
                            .iter()
                            .map(|target_arc| KCasWord::new(target_arc.as_ref(), 0, i))
                            .collect();
                        let kcas_words: [KCasWord; NUM_WORDS] = kcas_words.try_into().unwrap();
                        wrapper.kcas(kcas_words)
                    });
                    handle
                })
                .collect();

            join_handles.into_iter().for_each(|join_handle| {
                let result: Result<(), Error> = join_handle.join().expect("A thread panicked");
                assert!(matches!(result, Ok(_)) || matches!(result, Err(Error::ValueWasNotExpectedValue)));
            });
        });

        assert!((0..NUM_THREADS).any(|i| targets
            .iter()
            .all(|target| target.load(Ordering::Acquire) == i)));
    }

    #[test]
    fn test_unsafe_state_wrapper_with_2_threads() {
        test_unsafe_state_wrapper::<2, 3>();
    }
    fn test_unsafe_state_wrapper<const NUM_THREADS: usize, const NUM_WORDS: usize>() {
        let state: State<NUM_THREADS, NUM_WORDS> = State::new();
        let state_pointer: NonNull<State<NUM_THREADS, NUM_WORDS>> = NonNull::from(&state);

        let targets: [Arc<AtomicUsize>; NUM_WORDS] =
            core::array::from_fn(|_| Arc::new(AtomicUsize::new(0)));

        thread::scope(|scope| {
            let join_handles: Vec<ScopedJoinHandle<Result<(), Error>>> = (0..NUM_THREADS)
                .map(|i| {
                    let mut wrapper: UnsafeStateWrapper<NUM_THREADS, NUM_WORDS> =
                        UnsafeStateWrapper::construct(state_pointer).unwrap();
                    let cloned_targets: Vec<Arc<AtomicUsize>> = targets
                        .iter()
                        .cloned()
                        .collect();
                    let handle: ScopedJoinHandle<Result<(), Error>> = scope.spawn(move || {
                        let cloned_targets = cloned_targets;
                        let kcas_words: Vec<KCasWord> = cloned_targets
                            .iter()
                            .map(|target_arc| KCasWord::new(target_arc.as_ref(), 0, i))
                            .collect();
                        let kcas_words: [KCasWord; NUM_WORDS] = kcas_words.try_into().unwrap();
                        wrapper.kcas(kcas_words)
                    });
                    handle
                })
                .collect();

            join_handles.into_iter().for_each(|join_handle| {
                let result: Result<(), Error> = join_handle.join().expect("A thread panicked");
                assert!(matches!(result, Ok(_)) || matches!(result, Err(Error::ValueWasNotExpectedValue)));
            });
        });

        assert!((0..NUM_THREADS).any(|i| targets
            .iter()
            .all(|target| target.load(Ordering::Acquire) == i)));
    }
}
