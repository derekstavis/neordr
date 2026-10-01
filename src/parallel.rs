//! Bounded independent reads with deterministic result ordering.

pub fn map<T: Sync, R: Send>(items: &[T], f: impl Fn(&T) -> R + Sync) -> Vec<R> {
    let mut results = Vec::with_capacity(items.len());
    // Discovery and snapshots must not create a thread per editor.
    for batch in items.chunks(4) {
        results.extend(std::thread::scope(|scope| {
            let handles: Vec<_> = batch.iter().map(|item| scope.spawn(|| f(item))).collect();
            handles
                .into_iter()
                .map(|h| h.join().expect("reader panicked"))
                .collect::<Vec<_>>()
        }));
    }
    results
}
