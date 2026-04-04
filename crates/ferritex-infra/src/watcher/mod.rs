use std::io;
use std::path::PathBuf;

mod polling_file_watcher;

pub trait FileWatcher {
    fn replace_paths<I>(&mut self, paths: I) -> io::Result<()>
    where
        Self: Sized,
        I: IntoIterator<Item = PathBuf>;

    fn poll_changes(&mut self) -> io::Result<Vec<PathBuf>>;
}

pub use polling_file_watcher::PollingFileWatcher;
