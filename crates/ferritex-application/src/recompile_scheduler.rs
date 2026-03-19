use std::collections::BTreeSet;
use std::path::PathBuf;

#[derive(Debug, Default)]
pub struct RecompileScheduler {
    compile_in_flight: bool,
    pending_changes: BTreeSet<PathBuf>,
}

impl RecompileScheduler {
    pub fn enqueue<I>(&mut self, changes: I)
    where
        I: IntoIterator<Item = PathBuf>,
    {
        self.pending_changes.extend(changes);
    }

    pub fn start_next(&mut self) -> Option<Vec<PathBuf>> {
        if self.compile_in_flight || self.pending_changes.is_empty() {
            return None;
        }

        self.compile_in_flight = true;
        let changes = self.pending_changes.iter().cloned().collect();
        self.pending_changes.clear();
        Some(changes)
    }

    pub fn finish_current(&mut self) {
        self.compile_in_flight = false;
    }

    pub fn has_pending_changes(&self) -> bool {
        !self.pending_changes.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::RecompileScheduler;

    #[test]
    fn coalesces_pending_paths_until_compile_starts() {
        let mut scheduler = RecompileScheduler::default();
        scheduler.enqueue([
            PathBuf::from("main.tex"),
            PathBuf::from("chap1.tex"),
            PathBuf::from("main.tex"),
        ]);

        let changes = scheduler.start_next().expect("queued changes");

        assert_eq!(
            changes,
            vec![PathBuf::from("chap1.tex"), PathBuf::from("main.tex")]
        );
    }

    #[test]
    fn does_not_start_while_compile_is_in_flight() {
        let mut scheduler = RecompileScheduler::default();
        scheduler.enqueue([PathBuf::from("main.tex")]);
        assert!(scheduler.start_next().is_some());

        scheduler.enqueue([PathBuf::from("chap2.tex")]);
        assert!(scheduler.start_next().is_none());
        assert!(scheduler.has_pending_changes());

        scheduler.finish_current();
        let changes = scheduler.start_next().expect("second compile");
        assert_eq!(changes, vec![PathBuf::from("chap2.tex")]);
    }
}
