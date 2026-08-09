use std::collections::VecDeque;

/// A user command routed from the TUI into the agent/generator layer.
///
/// Mirrors the command naming in `docs/blueprint.md` §4B (`Prompt`, `Stop`) for
/// spike fidelity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentCommand {
    /// New user message (steer/restart the stream). Carries the drained input.
    Prompt(String),
    /// Interrupt the current generation.
    Stop,
}

/// A bounded scrolling ring buffer of token lines.
///
/// Dropping oldest entries at `cap` keeps memory flat over a soak run. Peak
/// byte length is tracked so the "no memory growth" criterion is measurable.
#[derive(Debug, Clone)]
pub struct RingBuffer {
    entries: VecDeque<String>,
    cap: usize,
    current_bytes: usize,
    peak_bytes: usize,
}

impl RingBuffer {
    pub fn new(cap: usize) -> Self {
        assert!(cap > 0, "ring buffer capacity must be > 0");
        Self {
            entries: VecDeque::with_capacity(cap),
            cap,
            current_bytes: 0,
            peak_bytes: 0,
        }
    }

    /// Append a line, evicting the oldest entry if over capacity.
    pub fn push(&mut self, line: String) {
        self.current_bytes += line.len();
        if self.entries.len() == self.cap
            && let Some(old) = self.entries.pop_front()
        {
            self.current_bytes -= old.len();
        }
        self.entries.push_back(line);
        if self.current_bytes > self.peak_bytes {
            self.peak_bytes = self.current_bytes;
        }
    }

    #[allow(dead_code)] // exercised by unit tests; not consumed by the bin TUI path
    pub fn clear(&mut self) {
        self.entries.clear();
        self.current_bytes = 0;
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[allow(dead_code)] // exercised by unit tests; not consumed by the bin TUI path
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    #[allow(dead_code)] // exercised by unit tests; not consumed by the bin TUI path
    pub fn capacity(&self) -> usize {
        self.cap
    }

    pub fn iter(&self) -> impl Iterator<Item = &String> {
        self.entries.iter()
    }

    #[allow(dead_code)] // exercised by unit tests; not consumed by the bin TUI path
    pub fn current_bytes(&self) -> usize {
        self.current_bytes
    }

    /// Highest total byte length ever held in the buffer (peak buffered memory).
    pub fn peak_bytes(&self) -> usize {
        self.peak_bytes
    }
}

/// Bounds for the frame-time histogram (milliseconds).
pub const HISTOGRAM_BUCKET_MILLIS: [f64; 4] = [8.0, 16.0, 33.0, f64::MAX];

/// A rolling window over per-frame draw durations.
///
/// Records min/avg/max, a count of frames exceeding the 17 ms budget, and a
/// coarse histogram so the "consistently ≤ 17 ms" criterion is graded.
#[derive(Debug, Clone)]
pub struct FrameTimeWindow {
    samples: VecDeque<f64>,
    max_samples: usize,
    sum: f64,
    total_frames: u64,
    frames_over_17ms: u64,
    min: Option<f64>,
    max: Option<f64>,
    /// histogram[i] counts frames < HISTOGRAM_BUCKET_MILLIS[i]
    histogram: [u64; 4],
}

impl FrameTimeWindow {
    const DEFAULT_MAX_SAMPLES: usize = 512;

    pub fn new() -> Self {
        Self::with_max_samples(Self::DEFAULT_MAX_SAMPLES)
    }

    pub fn with_max_samples(max_samples: usize) -> Self {
        assert!(max_samples > 0, "frame window capacity must be > 0");
        Self {
            samples: VecDeque::with_capacity(max_samples),
            max_samples,
            sum: 0.0,
            total_frames: 0,
            frames_over_17ms: 0,
            min: None,
            max: None,
            histogram: [0; 4],
        }
    }

    pub fn record(&mut self, duration_millis: f64) {
        let duration = duration_millis.max(0.0);
        self.total_frames += 1;
        self.sum += duration;

        if self.samples.len() == self.max_samples
            && let Some(old) = self.samples.pop_front()
        {
            self.sum -= old;
        }
        self.samples.push_back(duration);

        self.min = Some(match self.min {
            Some(m) if m <= duration => m,
            _ => duration,
        });
        self.max = Some(match self.max {
            Some(m) if m >= duration => m,
            _ => duration,
        });

        if duration > 17.0 {
            self.frames_over_17ms += 1;
        }

        for (index, bucket) in HISTOGRAM_BUCKET_MILLIS.iter().enumerate() {
            if duration < *bucket {
                self.histogram[index] += 1;
                break;
            }
        }
    }

    #[allow(dead_code)] // exercised by unit tests; not consumed by the bin TUI path
    pub fn len(&self) -> usize {
        self.samples.len()
    }

    #[allow(dead_code)] // exercised by unit tests; not consumed by the bin TUI path
    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    pub fn total_frames(&self) -> u64 {
        self.total_frames
    }

    pub fn frames_over_17ms(&self) -> u64 {
        self.frames_over_17ms
    }

    pub fn average_millis(&self) -> f64 {
        if self.samples.is_empty() {
            return 0.0;
        }
        self.sum / self.samples.len() as f64
    }

    pub fn min_millis(&self) -> Option<f64> {
        self.min
    }

    pub fn max_millis(&self) -> Option<f64> {
        self.max
    }

    pub fn histogram(&self) -> [u64; 4] {
        self.histogram
    }
}

impl Default for FrameTimeWindow {
    fn default() -> Self {
        Self::new()
    }
}

/// The TUI's live state: streaming-scroll buffer, stream flag, input, and metrics.
pub struct App {
    pub ring: RingBuffer,
    pub streaming: bool,
    pub input: String,
    pub frame_time: FrameTimeWindow,
    pub last_keys: VecDeque<String>,
    pub peak_queue_depth: usize,
    pub current_queue_depth: usize,
}

impl App {
    pub fn new(ring_cap: usize) -> Self {
        Self {
            ring: RingBuffer::new(ring_cap),
            streaming: true,
            input: String::new(),
            frame_time: FrameTimeWindow::new(),
            last_keys: VecDeque::with_capacity(16),
            peak_queue_depth: 0,
            current_queue_depth: 0,
        }
    }

    pub fn record_key(&mut self, key: String) {
        if self.last_keys.len() == 16 {
            self.last_keys.pop_front();
        }
        self.last_keys.push_back(key);
    }

    pub fn record_queue_depth(&mut self, depth: usize) {
        self.current_queue_depth = depth;
        if depth > self.peak_queue_depth {
            self.peak_queue_depth = depth;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_buffer_drops_oldest_at_capacity() {
        let mut ring = RingBuffer::new(3);
        ring.push("a".into());
        ring.push("b".into());
        ring.push("c".into());
        ring.push("d".into());
        assert_eq!(ring.len(), 3);
        let items: Vec<&String> = ring.iter().collect();
        assert_eq!(items, vec!["b", "c", "d"]);
        assert_eq!(ring.capacity(), 3);
    }

    #[test]
    fn ring_buffer_current_bytes_tracks_evictions() {
        let mut ring = RingBuffer::new(2);
        ring.push("aaa".into());
        ring.push("bb".into());
        assert_eq!(ring.current_bytes(), 5);
        ring.push("cccc".into());
        // "aaa" evicted (3 bytes), 2 + 4 = 6 remaining.
        assert_eq!(ring.current_bytes(), 6);
        assert_eq!(ring.peak_bytes(), 6);
    }

    #[test]
    fn ring_buffer_clear_resets_bytes_but_keeps_peak() {
        let mut ring = RingBuffer::new(2);
        ring.push("aaa".into());
        ring.clear();
        assert_eq!(ring.len(), 0);
        assert_eq!(ring.current_bytes(), 0);
        assert_eq!(ring.peak_bytes(), 3);
    }

    #[test]
    fn ring_buffer_never_grows_beyond_capacity() {
        let mut ring = RingBuffer::new(8);
        for i in 0..100_000 {
            ring.push(format!("line {i}"));
        }
        assert_eq!(ring.len(), 8);
        assert!(ring.peak_bytes() <= 10_000);
    }

    #[test]
    #[should_panic]
    fn ring_buffer_rejects_zero_capacity() {
        let _ = RingBuffer::new(0);
    }

    #[test]
    fn frame_window_records_min_avg_max() {
        let mut w = FrameTimeWindow::new();
        w.record(5.0);
        w.record(10.0);
        w.record(20.0);
        assert_eq!(w.total_frames(), 3);
        assert_eq!(w.min_millis(), Some(5.0));
        assert_eq!(w.max_millis(), Some(20.0));
        assert!((w.average_millis() - (35.0 / 3.0)).abs() < f64::EPSILON);
        assert_eq!(w.frames_over_17ms(), 1);
    }

    #[test]
    fn frame_window_evicts_oldest_sample() {
        let mut w = FrameTimeWindow::with_max_samples(2);
        w.record(10.0);
        w.record(20.0);
        w.record(30.0);
        assert_eq!(w.len(), 2);
        assert_eq!(w.total_frames(), 3);
        // Only the last two samples (20, 30) remain: avg = 25.
        assert!((w.average_millis() - 25.0).abs() < f64::EPSILON);
    }

    #[test]
    fn frame_window_histogram_buckets() {
        let mut w = FrameTimeWindow::new();
        w.record(5.0); // <8
        w.record(12.0); // 8..16
        w.record(20.0); // 16..33
        w.record(100.0); // >33
        assert_eq!(w.histogram(), [1, 1, 1, 1]);
    }

    #[test]
    fn frame_window_empty_state() {
        let w = FrameTimeWindow::new();
        assert_eq!(w.average_millis(), 0.0);
        assert_eq!(w.min_millis(), None);
        assert_eq!(w.max_millis(), None);
        assert_eq!(w.frames_over_17ms(), 0);
    }
}
