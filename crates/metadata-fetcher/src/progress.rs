use indicatif::{ProgressBar, ProgressStyle};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub struct CrawlProgress {
    bar: ProgressBar,
    start: Instant,
    processed: AtomicU64,
}

impl CrawlProgress {
    pub fn new(total: u64) -> Arc<Self> {
        let bar = ProgressBar::new(total);
        bar.set_style(
            ProgressStyle::default_bar()
                .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} ({percent}%) {msg}")
                .unwrap()
                .progress_chars("#>-"),
        );
        bar.enable_steady_tick(Duration::from_secs(1));

        Arc::new(Self {
            bar,
            start: Instant::now(),
            processed: AtomicU64::new(0),
        })
    }

    pub fn inc(&self, count: u64) {
        let total = self.processed.fetch_add(count, Ordering::Relaxed) + count;
        self.bar.inc(count);

        if total % 50_000 == 0 {
            let rate = total as f64 / self.start.elapsed().as_secs_f64();
            let remaining = self.bar.length().unwrap_or(0).saturating_sub(total);
            let eta_secs = if rate > 0.0 {
                remaining as f64 / rate
            } else {
                0.0
            };
            self.bar.set_message(format!(
                "{:.0} domains/sec | ETA: {:.0}m",
                rate,
                eta_secs / 60.0
            ));
        }
    }

    pub fn finish(&self) {
        let total = self.processed.load(Ordering::Relaxed);
        let elapsed = self.start.elapsed();
        let rate = total as f64 / elapsed.as_secs_f64();
        self.bar.finish_with_message(format!(
            "Done! {} domains in {:.1}s ({:.0}/sec)",
            total,
            elapsed.as_secs_f64(),
            rate
        ));
    }
}
