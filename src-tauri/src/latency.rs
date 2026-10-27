use parking_lot::Mutex;
use serde::Serialize;
use std::{
    collections::{BTreeMap, VecDeque},
    time::Instant,
};

#[derive(Clone, Serialize)]
pub struct Report {
    pub turn: u32,
    pub outcome: String,
    pub milliseconds_from_speech_end: BTreeMap<String, u64>,
}
struct Timing {
    start: Instant,
    marks: BTreeMap<String, u64>,
}
#[derive(Default)]
pub struct LatencyBook {
    inner: Mutex<(BTreeMap<u32, Timing>, VecDeque<Report>)>,
}
impl LatencyBook {
    pub fn begin(&self, turn: u32, speech_end: Instant) {
        let mut marks = BTreeMap::new();
        marks.insert("speech_end".into(), 0);
        marks.insert(
            "final_transcript".into(),
            speech_end.elapsed().as_millis() as u64,
        );
        self.inner.lock().0.insert(
            turn,
            Timing {
                start: speech_end,
                marks,
            },
        );
    }
    pub fn mark(&self, turn: u32, name: &str) {
        if let Some(timing) = self.inner.lock().0.get_mut(&turn) {
            timing
                .marks
                .entry(name.into())
                .or_insert_with(|| timing.start.elapsed().as_millis() as u64);
        }
    }
    pub fn finish(&self, turn: u32, outcome: &str) {
        let mut inner = self.inner.lock();
        if let Some(timing) = inner.0.remove(&turn) {
            let report = Report {
                turn,
                outcome: outcome.into(),
                milliseconds_from_speech_end: timing.marks,
            };
            tracing::info!(turn,outcome,ms=?report.milliseconds_from_speech_end,"turn_latency");
            crate::diagnostics::record(
                "latency",
                &serde_json::to_string(&report).unwrap_or_default(),
                Some(turn),
            );
            inner.1.push_back(report);
            while inner.1.len() > 20 {
                inner.1.pop_front();
            }
        }
    }
    pub fn recent(&self) -> Vec<Report> {
        self.inner.lock().1.iter().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn retains_twenty_and_finishes_once() {
        let b = LatencyBook::default();
        for id in 0..25 {
            b.begin(id, Instant::now());
            b.mark(id, "first_token");
            b.finish(id, "completed");
            b.finish(id, "again");
        }
        let r = b.recent();
        assert_eq!(r.len(), 20);
        assert_eq!(r[0].turn, 5);
        assert_eq!(r[19].outcome, "completed");
        assert!(r[19]
            .milliseconds_from_speech_end
            .contains_key("first_token"));
    }
}
