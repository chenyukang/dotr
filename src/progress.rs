use std::path::Path;

pub trait BackupProgress {
    fn start(&mut self, _repo_root: &Path) {}
    fn phase(&mut self, _message: &str) {}
    fn source(&mut self, _source: &Path) {}
    fn scanned(&mut self, _scanned: usize, _current: &Path) {}
}

#[derive(Debug, Default)]
pub struct NoopProgress;

impl BackupProgress for NoopProgress {}

#[derive(Debug)]
pub struct StderrProgress {
    next_scan_report_at: usize,
}

impl StderrProgress {
    pub fn new() -> Self {
        Self {
            next_scan_report_at: 200,
        }
    }
}

impl Default for StderrProgress {
    fn default() -> Self {
        Self::new()
    }
}

impl BackupProgress for StderrProgress {
    fn start(&mut self, repo_root: &Path) {
        eprintln!("backup: repo {}", repo_root.display());
    }

    fn phase(&mut self, message: &str) {
        eprintln!("backup: {message}");
    }

    fn source(&mut self, source: &Path) {
        self.next_scan_report_at = 200;
        eprintln!("backup: scanning {}", source.display());
    }

    fn scanned(&mut self, scanned: usize, current: &Path) {
        if scanned < self.next_scan_report_at {
            return;
        }

        eprintln!(
            "backup: scanned {scanned} entries, now {}",
            current.display()
        );
        self.next_scan_report_at += 200;
    }
}
