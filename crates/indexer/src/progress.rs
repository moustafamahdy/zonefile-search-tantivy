use indicatif::{ProgressBar, ProgressStyle};
use std::time::{Duration, Instant};

/// Progress tracker for indexing operations
pub struct IndexProgress {
    bar: ProgressBar,
    start: Instant,
    last_log: Instant,
    processed: u64,
}

impl IndexProgress {
    /// Create a new progress tracker with estimated total
    pub fn new(estimated_total: u64) -> Self {
        let bar = ProgressBar::new(estimated_total);
        bar.set_style(
            ProgressStyle::default_bar()
                .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} ({percent}%) {msg}")
                .unwrap()
                .progress_chars("#>-"),
        );

        Self {
            bar,
            start: Instant::now(),
            last_log: Instant::now(),
            processed: 0,
        }
    }

    /// Create an unbounded progress spinner
    pub fn spinner() -> Self {
        let bar = ProgressBar::new_spinner();
        bar.set_style(
            ProgressStyle::default_spinner()
                .template("{spinner:.green} [{elapsed_precise}] {pos} domains processed {msg}")
                .unwrap(),
        );

        Self {
            bar,
            start: Instant::now(),
            last_log: Instant::now(),
            processed: 0,
        }
    }

    /// Increment progress by count
    pub fn inc(&mut self, count: u64) {
        self.processed += count;
        self.bar.inc(count);

        // Update message every 5 seconds
        if self.last_log.elapsed() > Duration::from_secs(5) {
            let rate = self.processed as f64 / self.start.elapsed().as_secs_f64();
            self.bar.set_message(format!("({:.0} docs/sec)", rate));
            self.last_log = Instant::now();
        }
    }

    /// Set a custom message
    pub fn set_message(&self, msg: impl Into<String>) {
        self.bar.set_message(msg.into());
    }

    /// Finish with a final message
    pub fn finish(&self) {
        let elapsed = self.start.elapsed();
        let rate = self.processed as f64 / elapsed.as_secs_f64();

        self.bar.finish_with_message(format!(
            "Done! {} domains in {:.1}s ({:.0} docs/sec)",
            self.processed,
            elapsed.as_secs_f64(),
            rate
        ));
    }

    /// Get current count
    pub fn count(&self) -> u64 {
        self.processed
    }

    /// Get elapsed time
    pub fn elapsed(&self) -> Duration {
        self.start.elapsed()
    }
}
