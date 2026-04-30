use std::time::Instant;

#[derive(Clone, Debug, PartialEq)]
pub struct TimingStat {
    pub filename: String,
    pub key: String,
    pub time_ms: f64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Timing {
    stats: Vec<TimingStat>,
}

impl Timing {
    pub fn measure<T>(
        &mut self,
        filename: impl Into<String>,
        key: impl Into<String>,
        f: impl FnOnce() -> T,
    ) -> T {
        let filename = filename.into();
        let key = key.into();
        let start = Instant::now();
        let value = f();
        self.stats.push(TimingStat {
            filename,
            key,
            time_ms: start.elapsed().as_secs_f64() * 1000.0,
        });
        value
    }

    pub fn push(&mut self, stat: TimingStat) {
        self.stats.push(stat);
    }

    pub fn merge(&mut self, other: Timing) {
        self.stats.extend(other.stats);
    }

    pub fn stats(&self) -> &[TimingStat] {
        &self.stats
    }
}
