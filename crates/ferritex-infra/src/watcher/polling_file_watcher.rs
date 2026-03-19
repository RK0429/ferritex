use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

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

#[derive(Debug, Default)]
pub struct PollingFileWatcher {
    tracked: BTreeMap<PathBuf, FileFingerprint>,
}

impl PollingFileWatcher {
    pub fn new<I>(paths: I) -> io::Result<Self>
    where
        I: IntoIterator<Item = PathBuf>,
    {
        let mut watcher = Self::default();
        watcher.replace_paths(paths)?;
        Ok(watcher)
    }

    pub fn replace_paths<I>(&mut self, paths: I) -> io::Result<()>
    where
        I: IntoIterator<Item = PathBuf>,
    {
        let mut tracked = BTreeMap::new();
        for path in paths {
            tracked.insert(path.clone(), FileFingerprint::capture(&path)?);
        }
        self.tracked = tracked;
        Ok(())
    }

    pub fn poll_changes(&mut self) -> io::Result<Vec<PathBuf>> {
        let mut changed = Vec::new();

        for (path, fingerprint) in &mut self.tracked {
            let latest = FileFingerprint::capture(path)?;
            if *fingerprint != latest {
                *fingerprint = latest;
                changed.push(path.clone());
            }
        }

        Ok(changed)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::PollingFileWatcher;

    #[test]
    fn detects_file_modifications() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let path = dir.path().join("main.tex");
        std::fs::write(&path, "before").expect("write file");

        let mut watcher = PollingFileWatcher::new([path.clone()]).expect("create watcher");

        std::thread::sleep(Duration::from_millis(5));
        std::fs::write(&path, "after").expect("rewrite file");

        let changed = watcher.poll_changes().expect("poll changes");
        assert_eq!(changed, vec![path]);
    }

    #[test]
    fn replaces_tracked_paths() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let first = dir.path().join("first.tex");
        let second = dir.path().join("second.tex");
        std::fs::write(&first, "first").expect("write first");
        std::fs::write(&second, "second").expect("write second");

        let mut watcher = PollingFileWatcher::new([first]).expect("create watcher");
        watcher
            .replace_paths([second.clone()])
            .expect("replace watcher paths");

        std::thread::sleep(Duration::from_millis(5));
        std::fs::write(&second, "changed").expect("rewrite second");

        let changed = watcher.poll_changes().expect("poll changes");
        assert_eq!(changed, vec![second]);
    }
}
