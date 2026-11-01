//! Private measurements of real lifecycle effects, never progress estimates.
//! Each synchronous measurement owns its thread-local observation scope. Other
//! tests and nested scopes cannot contribute to its counts.
use std::cell::Cell;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Serialize)]
pub(crate) struct Counts {
    pub(crate) command_entries: usize,
    pub(crate) batch_entries: usize,
    pub(crate) graph_operations: usize,
    pub(crate) metadata_retrievals: usize,
    pub(crate) artifact_requests: usize,
    pub(crate) materializations: usize,
    pub(crate) activations: usize,
    pub(crate) durable_decisions: usize,
}

thread_local! {
    static ACTIVE: Cell<Option<Counts>> = const { Cell::new(None) };
}

pub(crate) fn observe(update: impl FnOnce(&mut Counts)) {
    ACTIVE.with(|active| {
        if let Some(mut counts) = active.get() {
            update(&mut counts);
            active.set(Some(counts));
        }
    });
}

pub(crate) fn measure<T>(run: impl FnOnce() -> T) -> (T, Counts) {
    struct Restore(Option<Counts>);
    impl Drop for Restore {
        fn drop(&mut self) {
            ACTIVE.with(|active| active.set(self.0));
        }
    }
    let restore = Restore(ACTIVE.with(|active| active.replace(Some(Counts::default()))));
    let result = run();
    let counts = ACTIVE.with(|active| active.get().expect("active measurement"));
    drop(restore);
    (result, counts)
}

#[test]
fn measurements_are_isolated_across_threads_nested_scopes_and_panics() {
    let (_, outer) = measure(|| {
        observe(|counts| counts.graph_operations += 1);
        let (_, inner) = measure(|| observe(|counts| counts.graph_operations += 2));
        assert_eq!(inner.graph_operations, 2);
        std::thread::spawn(|| {
            observe(|counts| counts.graph_operations += 100);
            assert_eq!(measure(|| ()).1, Counts::default());
        })
        .join()
        .unwrap();
        let panic = std::panic::catch_unwind(|| measure(|| panic!("measurement interrupted")));
        assert!(panic.is_err());
        observe(|counts| counts.graph_operations += 1);
    });
    assert_eq!(outer.graph_operations, 2);
    assert_eq!(measure(|| ()).1, Counts::default());
}
