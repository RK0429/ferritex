use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use super::FileWatcher;

#[derive(Debug, Clone, PartialEq, Eq)]
struct FileFingerprint {
    exists: bool,
    length: u64,
    modified: Option<SystemTime>,
}

impl FileFingerprint {
    fn capture(path: &Path) -> io::Result<Self> {
        match std::fs::metadata(path) {
            Ok(metadata) => Ok(Self {
                exists: true,
                length: metadata.len(),
                modified: metadata.modified().ok(),
            }),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Self {
                exists: false,
                length: 0,
                modified: None,
            }),
            Err(error) => Err(error),
        }
    }
}

#[derive(Debug)]
struct TrackedFile {
    fingerprint: FileFingerprint,
    last_emitted_at: Option<Instant>,
}

impl TrackedFile {
    fn capture(path: &Path, last_emitted_at: Option<Instant>) -> io::Result<Self> {
        Ok(Self {
            fingerprint: FileFingerprint::capture(path)?,
            last_emitted_at,
        })
    }

    fn should_emit(&self, debounce_window: Duration, now: Instant) -> bool {
        if debounce_window.is_zero() {
            return true;
        }

        // Debounce is keyed by the last emitted event, not the latest observed fingerprint.
        // `poll_changes()` updates the fingerprint before calling this gate, so suppressed
        // changes are intentionally forgotten instead of being replayed after the window ends.
        self.last_emitted_at
            .map(|last| now.duration_since(last) >= debounce_window)
            .unwrap_or(true)
    }
}

#[derive(Debug)]
pub struct PollingFileWatcher {
    tracked: BTreeMap<PathBuf, TrackedFile>,
    debounce_window: Duration,
}

impl Default for PollingFileWatcher {
    fn default() -> Self {
        Self {
            tracked: BTreeMap::new(),
            debounce_window: Duration::ZERO,
        }
    }
}

impl PollingFileWatcher {
    pub fn new<I>(paths: I) -> io::Result<Self>
    where
        I: IntoIterator<Item = PathBuf>,
    {
        Self::with_debounce(paths, Duration::ZERO)
    }

    pub fn with_debounce<I>(paths: I, debounce_window: Duration) -> io::Result<Self>
    where
        I: IntoIterator<Item = PathBuf>,
    {
        let mut watcher = Self {
            debounce_window,
            ..Self::default()
        };
        watcher.replace_paths(paths)?;
        Ok(watcher)
    }

    pub fn replace_paths<I>(&mut self, paths: I) -> io::Result<()>
    where
        I: IntoIterator<Item = PathBuf>,
    {
        let mut previous = std::mem::take(&mut self.tracked);
        let mut tracked = BTreeMap::new();
        for path in paths {
            let canonical_path = path.canonicalize()?;
            let last_emitted_at = previous
                .remove(&canonical_path)
                .and_then(|tracked_file| tracked_file.last_emitted_at);
            tracked.insert(
                canonical_path.clone(),
                TrackedFile::capture(&canonical_path, last_emitted_at)?,
            );
        }
        self.tracked = tracked;
        Ok(())
    }

    /// Polls tracked paths and emits only the changes that survive the debounce gate.
    ///
    /// When `debounce_window` is non-zero, repeated changes to the same path inside the window
    /// are suppressed. The stored fingerprint is still updated immediately, so once the window
    /// expires a later poll with no new filesystem change observes "no change" and does not
    /// replay the suppressed event. This intentional change-loss tradeoff lets the watch loop
    /// skip intermediate states during rapid save bursts instead of recompiling each one.
    ///
    /// Current production code uses `PollingFileWatcher::new()`, which sets
    /// `debounce_window` to `Duration::ZERO`, so this behavior is presently opt-in.
    pub fn poll_changes(&mut self) -> io::Result<Vec<PathBuf>> {
        let mut changed = Vec::new();
        let now = Instant::now();

        for (path, tracked_file) in &mut self.tracked {
            let latest = FileFingerprint::capture(path)?;
            if tracked_file.fingerprint != latest {
                tracked_file.fingerprint = latest;
                if tracked_file.should_emit(self.debounce_window, now) {
                    tracked_file.last_emitted_at = Some(now);
                    changed.push(path.clone());
                }
            }
        }

        Ok(changed)
    }
}

impl FileWatcher for PollingFileWatcher {
    fn replace_paths<I>(&mut self, paths: I) -> io::Result<()>
    where
        Self: Sized,
        I: IntoIterator<Item = PathBuf>,
    {
        PollingFileWatcher::replace_paths(self, paths)
    }

    fn poll_changes(&mut self) -> io::Result<Vec<PathBuf>> {
        PollingFileWatcher::poll_changes(self)
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::Duration;

    use super::PollingFileWatcher;
    use crate::watcher::FileWatcher;

    fn poll_changes(watcher: &mut impl FileWatcher) -> Vec<PathBuf> {
        watcher.poll_changes().expect("poll changes")
    }

    fn replace_paths(watcher: &mut impl FileWatcher, paths: impl IntoIterator<Item = PathBuf>) {
        watcher.replace_paths(paths).expect("replace watcher paths");
    }

    #[test]
    fn detects_file_modifications() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let path = dir.path().join("main.tex");
        std::fs::write(&path, "before").expect("write file");
        let canonical_path = path.canonicalize().expect("canonical path");

        let mut watcher = PollingFileWatcher::new([path.clone()]).expect("create watcher");

        std::thread::sleep(Duration::from_millis(5));
        std::fs::write(&path, "after-after").expect("rewrite file");

        let changed = poll_changes(&mut watcher);
        assert_eq!(changed, vec![canonical_path]);
    }

    #[test]
    fn replaces_tracked_paths() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let first = dir.path().join("first.tex");
        let second = dir.path().join("second.tex");
        std::fs::write(&first, "first").expect("write first");
        std::fs::write(&second, "second").expect("write second");
        let canonical_second = second.canonicalize().expect("canonical second");

        let mut watcher = PollingFileWatcher::new([first]).expect("create watcher");
        replace_paths(&mut watcher, [second.clone()]);

        std::thread::sleep(Duration::from_millis(5));
        std::fs::write(&second, "changed-again").expect("rewrite second");

        let changed = poll_changes(&mut watcher);
        assert_eq!(changed, vec![canonical_second]);
    }

    #[test]
    fn suppresses_repeated_changes_within_debounce_window() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let path = dir.path().join("debounce.tex");
        std::fs::write(&path, "zero").expect("write file");
        let canonical_path = path.canonicalize().expect("canonical path");

        let mut watcher =
            PollingFileWatcher::with_debounce([path.clone()], Duration::from_millis(200))
                .expect("create watcher");

        std::thread::sleep(Duration::from_millis(5));
        std::fs::write(&path, "first-change").expect("write first change");
        assert_eq!(poll_changes(&mut watcher), vec![canonical_path.clone()]);

        std::thread::sleep(Duration::from_millis(5));
        std::fs::write(&path, "second-change").expect("write second change");
        assert!(poll_changes(&mut watcher).is_empty());

        std::thread::sleep(Duration::from_millis(210));
        std::fs::write(&path, "third-change-longer").expect("write third change");
        assert_eq!(poll_changes(&mut watcher), vec![canonical_path]);
    }

    #[test]
    fn debounce_drops_intermediate_changes_intentionally() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let path = dir.path().join("debounce-drop.tex");
        std::fs::write(&path, "zero").expect("write file");
        let canonical_path = path.canonicalize().expect("canonical path");

        let mut watcher =
            PollingFileWatcher::with_debounce([path.clone()], Duration::from_millis(200))
                .expect("create watcher");

        std::thread::sleep(Duration::from_millis(5));
        std::fs::write(&path, "first-change").expect("write first change");
        assert_eq!(poll_changes(&mut watcher), vec![canonical_path]);

        std::thread::sleep(Duration::from_millis(5));
        std::fs::write(&path, "second-change").expect("write second change");
        assert!(
            poll_changes(&mut watcher).is_empty(),
            "second change should be suppressed inside the debounce window"
        );

        std::thread::sleep(Duration::from_millis(210));
        assert!(
            poll_changes(&mut watcher).is_empty(),
            "suppressed change should not be replayed after the debounce window"
        );
    }

    #[test]
    fn supports_trait_objects_for_polling() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let path = dir.path().join("trait-object.tex");
        std::fs::write(&path, "before").expect("write file");
        let canonical_path = path.canonicalize().expect("canonical path");

        let mut watcher: Box<dyn FileWatcher> =
            Box::new(PollingFileWatcher::new([path.clone()]).expect("create watcher"));

        std::thread::sleep(Duration::from_millis(5));
        std::fs::write(&path, "after-after").expect("rewrite file");

        let changed = watcher.poll_changes().expect("poll changes");
        assert_eq!(changed, vec![canonical_path]);
    }

    #[cfg(unix)]
    #[test]
    fn normalizes_symlink_paths_to_canonical_targets() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().expect("create tempdir");
        let target = dir.path().join("target.tex");
        let symlink_path = dir.path().join("linked.tex");
        std::fs::write(&target, "before").expect("write target");
        symlink(&target, &symlink_path).expect("create symlink");
        let canonical_target = target.canonicalize().expect("canonical target");

        let mut watcher = PollingFileWatcher::new([symlink_path]).expect("create watcher");

        std::thread::sleep(Duration::from_millis(5));
        std::fs::write(&target, "after-after").expect("rewrite target");

        let changed = poll_changes(&mut watcher);
        assert_eq!(changed, vec![canonical_target]);
    }
}
