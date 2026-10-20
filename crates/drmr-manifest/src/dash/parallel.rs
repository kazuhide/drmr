use super::not_checked;
use crate::{DiagnosticSink, NotCheckedReason};
use std::{
    sync::atomic::{AtomicUsize, Ordering},
    thread,
};

const CHUNK: usize = 32;

/// One bounded sink per worker, one shared read-only DOM and no nested pool.
pub(super) fn run<T: Sync>(
    jobs: &[T],
    maximum: usize,
    sink: &mut DiagnosticSink<'_>,
    check: &(impl Fn(&T, &mut DiagnosticSink<'_>) + Sync),
) {
    let workers = maximum.min(jobs.len().div_ceil(CHUNK)).max(1);
    if workers == 1 {
        for job in jobs {
            check(job, sink);
        }
        return;
    }
    let next = AtomicUsize::new(0);
    thread::scope(|scope| {
        let mut handles = Vec::with_capacity(workers - 1);
        for _ in 1..workers {
            let mut child = sink.fork();
            let next = &next;
            match thread::Builder::new().spawn_scoped(scope, move || {
                consume(jobs, next, &mut child, check);
                child
            }) {
                Ok(handle) => handles.push(handle),
                Err(_) => not_checked(
                    sink,
                    "DASH-EXECUTION-LIMIT",
                    NotCheckedReason::ResourceLimit,
                    None,
                ),
            }
        }
        sink.workers_used = sink.workers_used.max(handles.len() + 1);
        consume(jobs, &next, sink, check);
        for handle in handles {
            match handle.join() {
                Ok(child) => sink.merge(child),
                Err(_) => not_checked(
                    sink,
                    "DASH-EXECUTION-LIMIT",
                    NotCheckedReason::ResourceLimit,
                    None,
                ),
            }
        }
    });
}

fn consume<T>(
    jobs: &[T],
    next: &AtomicUsize,
    sink: &mut DiagnosticSink<'_>,
    check: &impl Fn(&T, &mut DiagnosticSink<'_>),
) {
    loop {
        let start = next.fetch_add(CHUNK, Ordering::Relaxed);
        let Some(chunk) = jobs.get(start..jobs.len().min(start + CHUNK)) else {
            break;
        };
        if chunk.is_empty() {
            break;
        }
        for job in chunk {
            check(job, sink);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Document, DocumentId, Format};
    use std::sync::{Barrier, Mutex, atomic::AtomicBool};

    #[test]
    fn a_worker_failure_is_explicitly_incomplete_and_uses_the_execution_policy_reference() {
        let document = Document {
            id: DocumentId(1),
            format: Format::Dash,
            bytes: b"",
            base_uri: None,
        };
        let mut sink = DiagnosticSink::new(document, 1);
        let caller = thread::current().id();
        let barrier = Barrier::new(2);
        let caller_seen = AtomicBool::new(false);
        let worker_seen = AtomicBool::new(false);
        run(&[(); CHUNK * 2], 2, &mut sink, &|(), _| {
            let is_caller = thread::current().id() == caller;
            let first = if is_caller {
                &caller_seen
            } else {
                &worker_seen
            };
            if !first.swap(true, Ordering::Relaxed) {
                barrier.wait();
                assert!(is_caller, "causal worker failure");
            }
        });
        assert!(sink.incomplete);
        assert_eq!(sink.errors, 0);
        let coverage = &sink.coverage["DASH-EXECUTION-LIMIT"];
        assert_eq!(
            coverage.status,
            crate::CheckStatus::NotChecked(NotCheckedReason::ResourceLimit)
        );
        assert!(coverage.reference.url.ends_with("#resource-limits"));
        assert_eq!(coverage.reference.section, "resource-limits");
    }

    #[test]
    fn independent_tasks_execute_on_distinct_threads_inside_one_admitted_pool() {
        let document = Document {
            id: DocumentId(1),
            format: Format::Dash,
            bytes: b"",
            base_uri: None,
        };
        let mut sink = DiagnosticSink::new(document, 1);
        let barrier = Barrier::new(4);
        let threads = Mutex::new(Vec::new());
        run(&[(); CHUNK * 4], 4, &mut sink, &|(), _| {
            let current = thread::current().id();
            let first = {
                let mut ids = threads.lock().unwrap();
                if ids.contains(&current) {
                    false
                } else {
                    ids.push(current);
                    true
                }
            };
            if first {
                barrier.wait();
            }
        });
        assert_eq!(threads.lock().unwrap().len(), 4);
        assert_eq!(sink.workers_used, 4);
    }
}
