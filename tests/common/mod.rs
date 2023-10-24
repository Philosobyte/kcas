use kcas::err::Error;
use kcas::ArcStateWrapper;
use kcas::{ArcStateWrapperError, KCasWord, State};
use tracing::{debug, trace};

cfg_if::cfg_if! {
    if #[cfg(loom)] {
        pub(crate) use loom::sync::atomic::{AtomicUsize, Ordering};
        pub(crate) use loom::sync::Arc;
        pub(crate) use loom::thread;
    } else if #[cfg(feature = "shuttle")] {
        pub(crate) use shuttle::sync::atomic::{AtomicUsize, Ordering};
        pub(crate) use shuttle::sync::Arc;
        pub(crate) use shuttle::thread;
    } else if #[cfg(feature = "std")] {
        pub(crate) use std::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};
        pub(crate) use std::sync::Arc;
        pub(crate) use std::thread;
    }
}

pub(crate) fn concurrency_test<const NUM_THREADS: usize, const NUM_WORDS: usize>() {
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
                .map(|target_arc| target_arc.clone())
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
