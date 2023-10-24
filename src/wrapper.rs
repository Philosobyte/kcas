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

#[cfg(all(test, feature = "std", not(shuttle)))]
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
    use tracing::debug;

    #[test]
    fn test_arc_state_wrapper_num_threads() {
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
    fn test_ref_state_wrapper_num_threads() {
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
    fn test_unsafe_state_wrapper_num_threads() {
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
    fn test_safe_wrapper() {
        let state: State<2, 3> = State::new();
        println!("State after initialization: {state:?}");
        let state_arc: Arc<State<2, 3>> = Arc::new(state);

        let mut first_state_wrapper: ArcStateWrapper<2, 3> =
            ArcStateWrapper::construct(state_arc.clone()).unwrap();
        let mut second_state_wrapper: ArcStateWrapper<2, 3> =
            ArcStateWrapper::construct(state_arc).unwrap();

        assert_eq!(first_state_wrapper.thread_id, 1usize);
        assert_eq!(second_state_wrapper.thread_id, 2usize);

        let first_location: AtomicUsize = AtomicUsize::new(50);
        let second_location: AtomicUsize = AtomicUsize::new(70);
        let third_location: AtomicUsize = AtomicUsize::new(90);
        debug!("first_location before kcas: {first_location:?}");
        debug!("second_location before kcas: {second_location:?}");
        debug!("third_location before kcas: {third_location:?}");

        first_state_wrapper
            .kcas([
                KCasWord::new(&first_location, 50, 51),
                KCasWord::new(&second_location, 70, 71),
                KCasWord::new(&third_location, 90, 91),
            ])
            .unwrap();

        debug!("first_location before kcas: {first_location:?}");
        debug!("second_location before kcas: {second_location:?}");
        debug!("third_location before kcas: {third_location:?}");

        assert_eq!(first_location.load(Ordering::Acquire), 51);
        assert_eq!(second_location.load(Ordering::Acquire), 71);
        assert_eq!(third_location.load(Ordering::Acquire), 91);

        second_state_wrapper
            .kcas([
                KCasWord::new(&first_location, 51, 52),
                KCasWord::new(&second_location, 71, 72),
                KCasWord::new(&third_location, 91, 92),
            ])
            .unwrap();

        assert_eq!(first_location.load(Ordering::Acquire), 52);
        assert_eq!(second_location.load(Ordering::Acquire), 72);
        assert_eq!(third_location.load(Ordering::Acquire), 92);
    }

    #[test]
    fn test_ref_wrapper() {
        let state: State<2, 3> = State::new();
        println!("State after initialization: {state:?}");

        let mut first_state_wrapper: RefStateWrapper<2, 3> =
            RefStateWrapper::construct(&state).unwrap();
        let mut second_state_wrapper: RefStateWrapper<2, 3> =
            RefStateWrapper::construct(&state).unwrap();

        assert_eq!(first_state_wrapper.thread_id, 1usize);
        assert_eq!(second_state_wrapper.thread_id, 2usize);

        let first_location: AtomicUsize = AtomicUsize::new(50);
        let second_location: AtomicUsize = AtomicUsize::new(70);
        let third_location: AtomicUsize = AtomicUsize::new(90);
        debug!("first_location before kcas: {first_location:?}");
        debug!("second_location before kcas: {second_location:?}");
        debug!("third_location before kcas: {third_location:?}");

        first_state_wrapper
            .kcas([
                KCasWord::new(&first_location, 50, 51),
                KCasWord::new(&second_location, 70, 71),
                KCasWord::new(&third_location, 90, 91),
            ])
            .unwrap();

        debug!("first_location before kcas: {first_location:?}");
        debug!("second_location before kcas: {second_location:?}");
        debug!("third_location before kcas: {third_location:?}");

        assert_eq!(first_location.load(Ordering::Acquire), 51);
        assert_eq!(second_location.load(Ordering::Acquire), 71);
        assert_eq!(third_location.load(Ordering::Acquire), 91);

        second_state_wrapper
            .kcas([
                KCasWord::new(&first_location, 51, 52),
                KCasWord::new(&second_location, 71, 72),
                KCasWord::new(&third_location, 91, 92),
            ])
            .unwrap();

        assert_eq!(first_location.load(Ordering::Acquire), 52);
        assert_eq!(second_location.load(Ordering::Acquire), 72);
        assert_eq!(third_location.load(Ordering::Acquire), 92);
    }

    #[test]
    fn test_unsafe_wrapper() {
        let state: State<2, 3> = State::new();
        debug!("State after initialization: {state:?}");
        let state_pointer: NonNull<State<2, 3>> = NonNull::from(&state);
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

        first_unsafe_wrapper
            .kcas([
                KCasWord::new(&first_location, 50, 51),
                KCasWord::new(&second_location, 70, 71),
                KCasWord::new(&third_location, 90, 91),
            ])
            .unwrap();

        debug!("first_location before kcas: {first_location:?}");
        debug!("second_location before kcas: {second_location:?}");
        debug!("third_location before kcas: {third_location:?}");

        assert_eq!(first_location.load(Ordering::Acquire), 51);
        assert_eq!(second_location.load(Ordering::Acquire), 71);
        assert_eq!(third_location.load(Ordering::Acquire), 91);

        second_unsafe_wrapper
            .kcas([
                KCasWord::new(&first_location, 51, 52),
                KCasWord::new(&second_location, 71, 72),
                KCasWord::new(&third_location, 91, 92),
            ])
            .unwrap();

        assert_eq!(first_location.load(Ordering::Acquire), 52);
        assert_eq!(second_location.load(Ordering::Acquire), 72);
        assert_eq!(third_location.load(Ordering::Acquire), 92);
    }

    #[test]
    fn test_unsafe_wrapper_actually_unsafe() {
        let state: State<2, 3> = State::new();
        debug!("State after initialization: {state:?}");
        let state_pointer: NonNull<State<2, 3>> = NonNull::from(&state);
        let mut first_unsafe_wrapper: UnsafeStateWrapper<2, 3> =
            UnsafeStateWrapper::construct(state_pointer).unwrap();

        assert_eq!(first_unsafe_wrapper.thread_id, 1usize);

        let first_location: AtomicUsize = AtomicUsize::new(50);
        let second_location: AtomicUsize = AtomicUsize::new(70);
        let third_location: AtomicUsize = AtomicUsize::new(90);
        debug!("first_location before kcas: {first_location:?}");
        debug!("second_location before kcas: {second_location:?}");
        debug!("third_location before kcas: {third_location:?}");

        first_unsafe_wrapper
            .kcas([
                KCasWord::new(&first_location, 50, 51),
                KCasWord::new(&second_location, 70, 71),
                KCasWord::new(&third_location, 90, 91),
            ])
            .unwrap();

        debug!("first_location before kcas: {first_location:?}");
        debug!("second_location before kcas: {second_location:?}");
        debug!("third_location before kcas: {third_location:?}");

        assert_eq!(first_location.load(Ordering::Acquire), 51);
        assert_eq!(second_location.load(Ordering::Acquire), 71);
        assert_eq!(third_location.load(Ordering::Acquire), 91);
    }

    #[test]
    fn test_scoped_concurrency() {
        let state: State<2, 3> = State::new();
        println!("State after initialization: {state:?}");

        let mut first_state_wrapper: RefStateWrapper<2, 3> =
            RefStateWrapper::construct(&state).unwrap();
        let mut second_state_wrapper: RefStateWrapper<2, 3> =
            RefStateWrapper::construct(&state).unwrap();

        let first_location: Arc<AtomicUsize> = Arc::new(AtomicUsize::new(50));
        let second_location: Arc<AtomicUsize> = Arc::new(AtomicUsize::new(70));
        let third_location: Arc<AtomicUsize> = Arc::new(AtomicUsize::new(90));

        let first_location_clone: Arc<AtomicUsize> = first_location.clone();
        let second_location_clone: Arc<AtomicUsize> = second_location.clone();
        let third_location_clone: Arc<AtomicUsize> = third_location.clone();

        let first_location_clone_2: Arc<AtomicUsize> = first_location.clone();
        let second_location_clone_2: Arc<AtomicUsize> = second_location.clone();
        let third_location_clone_2: Arc<AtomicUsize> = third_location.clone();

        thread::scope(|scope| {
            let first_handle: ScopedJoinHandle<Result<(), Error>> = scope.spawn(|| {
                first_state_wrapper.kcas([
                    KCasWord::new(first_location_clone.as_ref(), 50, 51),
                    KCasWord::new(second_location_clone.as_ref(), 70, 71),
                    KCasWord::new(third_location_clone.as_ref(), 90, 91),
                ])
            });
            let second_handle: ScopedJoinHandle<Result<(), Error>> = scope.spawn(|| {
                second_state_wrapper.kcas([
                    KCasWord::new(first_location_clone_2.as_ref(), 50, 52),
                    KCasWord::new(second_location_clone_2.as_ref(), 70, 72),
                    KCasWord::new(third_location_clone_2.as_ref(), 90, 92),
                ])
            });

            let second_result: Result<(), Error> =
                second_handle.join().expect("The second thread panicked");
            let first_result: Result<(), Error> =
                first_handle.join().expect("The first thread panicked");

            debug!("first_result: {:?}", first_result);
            debug!("second_result: {:?}", second_result);

            assert!(
                (matches!(first_result, Ok(_))
                    && matches!(second_result, Err(Error::ValueWasNotExpectedValue)))
                    || (matches!(second_result, Ok(_))
                        && matches!(first_result, Err(Error::ValueWasNotExpectedValue)))
            );
        });

        debug!("first_location after kcas: {first_location:?}");
        debug!("second_location after kcas: {second_location:?}");
        debug!("third_location after kcas: {third_location:?}");

        assert!(
            (first_location.load(Ordering::Acquire) == 51
                && second_location.load(Ordering::Acquire) == 71
                && third_location.load(Ordering::Acquire) == 91)
                || (first_location.load(Ordering::Acquire) == 52
                    && second_location.load(Ordering::Acquire) == 72
                    && third_location.load(Ordering::Acquire) == 92)
        );
    }

    #[test]
    fn test_scoped_concurrency_v2() {
        let state: State<2, 3> = State::new();
        println!("State after initialization: {state:?}");

        let mut first_state_wrapper: RefStateWrapper<2, 3> =
            RefStateWrapper::construct(&state).unwrap();
        let mut second_state_wrapper: RefStateWrapper<2, 3> =
            RefStateWrapper::construct(&state).unwrap();

        let first_target: AtomicUsize = AtomicUsize::new(50);
        let second_target: AtomicUsize = AtomicUsize::new(70);
        let third_target: AtomicUsize = AtomicUsize::new(90);

        let first_kcas_word: KCasWord = KCasWord::new(&first_target, 50, 51);
        let second_kcas_word: KCasWord = KCasWord::new(&second_target, 70, 71);
        let third_kcas_word: KCasWord = KCasWord::new(&third_target, 90, 91);

        // let first_kcas_word_clone: KCasWord = first_kcas_word.clone();
        // let second_kcas_word_clone: KCasWord = second_kcas_word.clone();
        // let third_kcas_word_clone: KCasWord = third_kcas_word.clone();
        thread::scope(|scope| {
            let first_handle: ScopedJoinHandle<Result<(), Error>> = scope.spawn(|| {
                first_state_wrapper.kcas([
                    first_kcas_word.clone(),
                    second_kcas_word.clone(),
                    third_kcas_word.clone(),
                ])
            });
            let second_handle: ScopedJoinHandle<Result<(), Error>> = scope.spawn(|| {
                second_state_wrapper.kcas([
                    first_kcas_word.clone(),
                    second_kcas_word.clone(),
                    third_kcas_word.clone(),
                ])
            });

            let second_result: Result<(), Error> =
                second_handle.join().expect("The second thread panicked");
            let first_result: Result<(), Error> =
                first_handle.join().expect("The first thread panicked");

            debug!("first_result: {:?}", first_result);
            debug!("second_result: {:?}", second_result);

            assert!(
                (matches!(first_result, Ok(_))
                    && matches!(second_result, Err(Error::ValueWasNotExpectedValue)))
                    || (matches!(second_result, Ok(_))
                        && matches!(first_result, Err(Error::ValueWasNotExpectedValue)))
            );
        });

        debug!("first_location after kcas: {first_target:?}");
        debug!("second_location after kcas: {second_target:?}");
        debug!("third_location after kcas: {third_target:?}");

        assert!(
            (first_target.load(Ordering::Acquire) == 51
                && second_target.load(Ordering::Acquire) == 71
                && third_target.load(Ordering::Acquire) == 91)
                || (first_target.load(Ordering::Acquire) == 52
                    && second_target.load(Ordering::Acquire) == 72
                    && third_target.load(Ordering::Acquire) == 92)
        );
    }

    #[test]
    fn test_safe_concurrency() {
        let state: State<2, 3> = State::new();
        println!("State after initialization: {state:?}");
        let state_arc: Arc<State<2, 3>> = Arc::new(state);

        let mut first_state_wrapper: ArcStateWrapper<2, 3> =
            ArcStateWrapper::construct(state_arc.clone()).unwrap();
        let mut second_state_wrapper: ArcStateWrapper<2, 3> =
            ArcStateWrapper::construct(state_arc).unwrap();

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
        let second_result: Result<(), Error> =
            second_handle.join().expect("The second thread panicked");
        let first_result: Result<(), Error> =
            first_handle.join().expect("The first thread panicked");

        debug!("first_result: {:?}", first_result);
        debug!("second_result: {:?}", second_result);

        debug!("first_location after kcas: {first_location:?}");
        debug!("second_location after kcas: {second_location:?}");
        debug!("third_location after kcas: {third_location:?}");

        assert!(
            (matches!(first_result, Ok(_))
                && matches!(second_result, Err(Error::ValueWasNotExpectedValue)))
                || (matches!(second_result, Ok(_))
                    && matches!(first_result, Err(Error::ValueWasNotExpectedValue)))
        );
        assert!(
            (first_location.load(Ordering::Acquire) == 51
                && second_location.load(Ordering::Acquire) == 71
                && third_location.load(Ordering::Acquire) == 91)
                || (first_location.load(Ordering::Acquire) == 52
                    && second_location.load(Ordering::Acquire) == 72
                    && third_location.load(Ordering::Acquire) == 92)
        );
        assert!(
            first_location.load(Ordering::Acquire) == 51
                && second_location.load(Ordering::Acquire) == 71
                && third_location.load(Ordering::Acquire) == 91
        )
    }
}
