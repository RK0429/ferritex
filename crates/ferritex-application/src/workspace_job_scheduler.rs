use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

#[derive(Debug, Default)]
pub struct WorkspaceJobScheduler {
    workspace_locks: Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>,
}

impl WorkspaceJobScheduler {
    pub fn run<T>(&self, workspace_root: &Path, job: impl FnOnce() -> T) -> T {
        let workspace_key = normalize_workspace_key(workspace_root);
        let lock = {
            let mut locks = self
                .workspace_locks
                .lock()
                .expect("workspace lock registry poisoned");
            locks
                .entry(workspace_key)
                .or_insert_with(|| Arc::new(Mutex::new(())))
                .clone()
        };

        let _guard = lock.lock().expect("workspace lock poisoned");
        job()
    }
}

fn normalize_workspace_key(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Barrier};
    use std::thread;
    use std::time::Duration;

    use super::WorkspaceJobScheduler;

    #[test]
    fn serializes_jobs_within_same_workspace() {
        let scheduler = Arc::new(WorkspaceJobScheduler::default());
        let workspace = Arc::new(PathBuf::from("."));
        let barrier = Arc::new(Barrier::new(2));
        let active_jobs = Arc::new(AtomicUsize::new(0));
        let max_active = Arc::new(AtomicUsize::new(0));

        let handles = (0..2)
            .map(|_| {
                let scheduler = Arc::clone(&scheduler);
                let workspace = Arc::clone(&workspace);
                let barrier = Arc::clone(&barrier);
                let active_jobs = Arc::clone(&active_jobs);
                let max_active = Arc::clone(&max_active);

                thread::spawn(move || {
                    barrier.wait();
                    scheduler.run(&workspace, || {
                        let active = active_jobs.fetch_add(1, Ordering::SeqCst) + 1;
                        max_active.fetch_max(active, Ordering::SeqCst);
                        thread::sleep(Duration::from_millis(30));
                        active_jobs.fetch_sub(1, Ordering::SeqCst);
                    });
                })
            })
            .collect::<Vec<_>>();

        for handle in handles {
            handle.join().expect("join worker");
        }

        assert_eq!(max_active.load(Ordering::SeqCst), 1);
    }
}
