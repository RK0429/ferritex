use std::collections::BTreeSet;
use std::path::PathBuf;
use std::time::{Duration, Instant};

#[derive(Debug, Default)]
pub struct RecompileScheduler {
    compile_in_flight: bool,
    pending_changes: BTreeSet<PathBuf>,
    settle_window: Duration,
    last_enqueue_at: Option<Instant>,
}

impl RecompileScheduler {
    /// trailing-edge debounce を行うスケジューラを構築する。
    /// `enqueue` のたびに待機時間をリセットし、`window` の間イベントが止まったときだけ
    /// `start_next` が変更を払い出す。`Duration::ZERO` なら従来どおり即時発火する。
    pub fn with_settle_window(window: Duration) -> Self {
        Self {
            settle_window: window,
            ..Self::default()
        }
    }

    pub fn enqueue<I>(&mut self, changes: I)
    where
        I: IntoIterator<Item = PathBuf>,
    {
        self.pending_changes.extend(changes);
        self.last_enqueue_at = Some(Instant::now());
    }

    pub fn start_next(&mut self) -> Option<Vec<PathBuf>> {
        self.start_next_at(Instant::now())
    }

    pub(crate) fn start_next_at(&mut self, now: Instant) -> Option<Vec<PathBuf>> {
        if self.compile_in_flight || self.pending_changes.is_empty() {
            return None;
        }

        if !self.settle_window.is_zero()
            && self
                .last_enqueue_at
                .is_some_and(|last| now.duration_since(last) < self.settle_window)
        {
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
    use std::time::Duration;

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

    #[test]
    fn waits_for_settle_window_before_starting_compile() {
        let mut scheduler = RecompileScheduler::with_settle_window(Duration::from_millis(200));
        scheduler.enqueue([PathBuf::from("main.tex")]);
        let first_enqueue_at = scheduler.last_enqueue_at.expect("first enqueue");
        assert!(scheduler
            .start_next_at(first_enqueue_at + Duration::from_millis(199))
            .is_none());

        scheduler.enqueue([PathBuf::from("chap1.tex")]);
        let last_enqueue_at = scheduler.last_enqueue_at.expect("second enqueue");
        assert!(scheduler
            .start_next_at(last_enqueue_at + Duration::from_millis(199))
            .is_none());

        let changes = scheduler
            .start_next_at(last_enqueue_at + Duration::from_millis(200))
            .expect("queued changes after settle window");

        assert_eq!(
            changes,
            vec![PathBuf::from("chap1.tex"), PathBuf::from("main.tex")]
        );
    }

    #[test]
    fn flushes_all_rapid_enqueues_in_single_batch_after_settle_window() {
        let mut scheduler = RecompileScheduler::with_settle_window(Duration::from_millis(200));

        for index in 0..5 {
            scheduler.enqueue([PathBuf::from(format!("touch-{index}.tex"))]);
        }

        let last_enqueue_at = scheduler.last_enqueue_at.expect("rapid enqueue");
        let changes = scheduler
            .start_next_at(last_enqueue_at + Duration::from_millis(200))
            .expect("batched changes after settle window");

        assert_eq!(
            changes,
            vec![
                PathBuf::from("touch-0.tex"),
                PathBuf::from("touch-1.tex"),
                PathBuf::from("touch-2.tex"),
                PathBuf::from("touch-3.tex"),
                PathBuf::from("touch-4.tex"),
            ]
        );
    }
}
