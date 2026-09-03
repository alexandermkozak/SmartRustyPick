//! Verdicts: the one place a number becomes advice.
//!
//! `FILE.STATS` and `LIST.INDEXES` used to answer "how big is this" and leave
//! the reader to decide whether the answer was bad. A largest group of 4 MB
//! against a smallest of 96 KB is either fine or the whole problem, and nothing
//! in the reply said which. A number nobody knows how to read is not
//! information.
//!
//! So every derived measure carries a [`Verdict`] and the threshold that
//! produced it, and both are decided *here*, on the server. Not in the
//! dashboard: the CLI, the remote protocol and the browser all describe the
//! same file, and three copies of "5% is the line" is three chances to
//! disagree. A client renders what it was told.
//!
//! # Reading a measure
//!
//! * [`Verdict::Good`] - nothing to do. Carries a detail anyway, because
//!   "healthy, and here is why" is what makes the bad case legible later.
//! * [`Verdict::Watch`] - not wrong now, and heading somewhere. A file about to
//!   rehash, an index nothing has queried yet.
//! * [`Verdict::Act`] - something is costing more than it should, and the
//!   detail says what to do about it.
//!
//! A measure that cannot be judged - a skew ratio over four records, an index
//! usage count on a server that started a second ago - reports `Good` and says
//! in its detail that it is not yet worth reading. Inventing a verdict from too
//! little data is how a dashboard teaches people to ignore it.

use serde::{Deserialize, Serialize};

/// Every threshold in this file, gathered so the numbers are arguable in one
/// place rather than scattered through the code that applies them.
///
/// They are guesses informed by the shape of the format, not measurements, and
/// they are written down together so that improving one is a small change.
pub mod thresholds {
    /// Largest group over mean group, above which the hash is not spreading
    /// records evenly. A write to an over-full group rewrites all of it, so
    /// skew is the failure mode that costs, not size.
    pub const SKEW_WATCH: f64 = 3.0;
    pub const SKEW_ACT: f64 = 6.0;

    /// A group holding more than this multiple of the mean is "overweight" and
    /// is counted. Lower than [`SKEW_WATCH`] on purpose: one outlier is noise,
    /// and a *count* of mild outliers is the thing that says the hash itself is
    /// not doing its job.
    pub const OVERWEIGHT_FACTOR: f64 = 2.0;

    /// Mean records per group below which skew says nothing. On four records
    /// spread over eight groups the largest group is three times the mean and
    /// that is simply what small numbers look like.
    pub const SKEW_MIN_MEAN: f64 = 4.0;

    /// Records against the modulus' capacity. Past this the next flush picks a
    /// larger modulus, which rewrites every group.
    pub const LOAD_FACTOR_WATCH: f64 = 0.85;

    /// Below this the modulus is far larger than the records need, which costs
    /// a directory of near-empty files. It only ever shrinks back on a flush.
    pub const LOAD_FACTOR_SPARSE: f64 = 0.15;

    /// Share of a file one index value may cover before indexing that value is
    /// worse than not indexing it: the lookup hands the scan behind it most of
    /// the file anyway, and the entry is the most expensive one to maintain.
    pub const DOMINANT_SHARE_ACT: f64 = 0.25;
    pub const DOMINANT_SHARE_WATCH: f64 = 0.10;

    /// A posting list shorter than this is cheap however large a share of a
    /// small file it is. Stops a four-record file reporting a crisis.
    pub const DOMINANT_MIN_POSTINGS: u64 = 10;

    /// Records an average lookup may hand back before the index is barely
    /// narrowing anything, as a share of the file.
    pub const LOOKUP_SHARE_WATCH: f64 = 0.25;

    /// Of the candidates an index produced, the share that survived the filter.
    /// Below this the index is answering with far more than the query wanted.
    pub const PRECISION_WATCH: f64 = 0.10;

    /// Lookups below which the usage counters say nothing yet - they are reset
    /// when the server starts, so a quiet minute is not evidence.
    pub const USAGE_MIN_LOOKUPS: u64 = 1;

    /// Buckets a group distribution is summarised into. Enough to see a shape,
    /// few enough that a file with a modulus of 65,536 still sends a small reply.
    pub const DISTRIBUTION_BUCKETS: usize = 16;

    /// Values an index histogram returns by default.
    pub const HISTOGRAM_DEFAULT: usize = 10;
    /// Most an `INDEX.STATS` caller may ask for, so one request cannot ask the
    /// server to sort and send every distinct value it holds.
    pub const HISTOGRAM_MAX: usize = 200;
}

/// What a measure says to do about itself.
///
/// Ordered worst-last so that [`Health::verdict`] is a `max` over its measures.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    #[default]
    Good,
    Watch,
    Act,
}

impl Verdict {
    pub fn as_str(self) -> &'static str {
        match self {
            Verdict::Good => "good",
            Verdict::Watch => "watch",
            Verdict::Act => "act",
        }
    }

    /// The worse of two verdicts, which is what a roll-up reports.
    pub fn worse(self, other: Verdict) -> Verdict {
        self.max(other)
    }
}

impl std::fmt::Display for Verdict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One thing worth knowing about a file or an index, and what it means.
///
/// `value` is formatted here rather than in each client for the same reason the
/// verdict is: the CLI and the dashboard should not be able to round the same
/// ratio differently. The raw numbers stay on the statistics structs for a
/// client that wants to draw them.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
pub struct Measure {
    /// Stable identifier. A client may key layout off this; it may not key off
    /// `label`, which is prose.
    pub id: String,
    /// Short name for a person.
    pub label: String,
    /// The measurement, already formatted.
    pub value: String,
    pub verdict: Verdict,
    /// The rule that produced the verdict, in the form a reader can argue with.
    pub threshold: String,
    /// What it means, and for anything but `Good`, what to do about it.
    pub detail: String,
}

impl Measure {
    pub fn new(
        id: &str,
        label: &str,
        value: impl Into<String>,
        verdict: Verdict,
        threshold: &str,
        detail: impl Into<String>,
    ) -> Self {
        Measure {
            id: id.to_string(),
            label: label.to_string(),
            value: value.into(),
            verdict,
            threshold: threshold.to_string(),
            detail: detail.into(),
        }
    }
}

/// A set of measures and the worst verdict among them.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
pub struct Health {
    pub verdict: Verdict,
    pub measures: Vec<Measure>,
}

impl Health {
    pub fn of(measures: Vec<Measure>) -> Self {
        let verdict = measures
            .iter()
            .map(|measure| measure.verdict)
            .fold(Verdict::Good, Verdict::worse);
        Health { verdict, measures }
    }

    /// The measures that are not `Good`, worst first. What a summary line says.
    pub fn concerns(&self) -> Vec<&Measure> {
        let mut concerns: Vec<&Measure> = self
            .measures
            .iter()
            .filter(|measure| measure.verdict != Verdict::Good)
            .collect();
        concerns.sort_by(|a, b| b.verdict.cmp(&a.verdict));
        concerns
    }
}

/// A verdict without the measures behind it.
///
/// What a *listing* carries. `LIST.FILES` and `LIST.ACCOUNTS` answer "which of
/// these is worth opening", and answering it must not cost what opening one
/// costs: this is derived from section metadata and index `state` files alone,
/// never from the group trailers and never from a record. The full measures
/// arrive with `FILE.STATS`.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
pub struct HealthSummary {
    pub verdict: Verdict,
    /// Short phrases naming what is wrong, empty when nothing is.
    pub reasons: Vec<String>,
}

impl HealthSummary {
    pub fn good() -> Self {
        HealthSummary::default()
    }

    pub fn note(&mut self, verdict: Verdict, reason: impl Into<String>) {
        self.verdict = self.verdict.worse(verdict);
        self.reasons.push(reason.into());
    }

    /// Rolls one summary into another, for an account over its files.
    pub fn absorb(&mut self, other: &HealthSummary) {
        self.verdict = self.verdict.worse(other.verdict);
    }
}

/// `x.y` with one decimal, or an integer when it is one. Ratios read better
/// without a trailing `.0` on them.
pub fn ratio(value: f64) -> String {
    if !value.is_finite() {
        return "—".to_string();
    }
    if (value - value.round()).abs() < 0.05 {
        format!("{}", value.round() as i64)
    } else {
        format!("{:.1}", value)
    }
}

/// A fraction as a whole-number percentage.
pub fn percent(value: f64) -> String {
    if !value.is_finite() {
        return "—".to_string();
    }
    format!("{:.0}%", value * 100.0)
}

/// Bytes as a person reads them. Matches the dashboard's own formatter, so a
/// size the server words and one the browser words look the same.
pub fn bytes(value: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut size = value as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{} B", value)
    } else {
        format!("{:.1} {}", size, UNITS[unit])
    }
}

/// The verdicts on one file.
///
/// Reads only what [`FileStats`] already carries, all of which came from the
/// section metadata, the group trailers and the index sections - so a file's
/// health costs no record read, which is the property `docs/web_dashboard.md`
/// promises about this whole view.
///
/// [`FileStats`]: crate::db::models::FileStats
pub fn file_health(stats: &crate::db::models::FileStats) -> Health {
    use thresholds as t;
    let mut measures = Vec::new();
    let groups = &stats.group_records;

    // Format and integrity first: they are the two that used to render as
    // neutral rows reading "no" and "legacy flat file", when both mean the file
    // is not yet protected by the current format's guarantees.
    measures.push(if stats.legacy {
        Measure::new(
            "format",
            "Storage format",
            "legacy flat file",
            Verdict::Act,
            "any file still in the pre-hashfile flat format",
            "Every write rewrites the whole file, and there are no per-group checksums, so a torn \
             write reads back as records that simply do not exist. The next flush converts it: \
             write to the file, or flush the database.",
        )
    } else {
        Measure::new(
            "format",
            "Storage format",
            "hashed",
            Verdict::Good,
            "any file still in the pre-hashfile flat format",
            "Records are spread over hash groups, so a write rewrites one group rather than the file.",
        )
    });

    measures.push(if stats.checksums {
        Measure::new(
            "checksums",
            "Per-group checksums",
            "yes",
            Verdict::Good,
            "a section written before the format appended a checksum trailer",
            "Every group carries a record count and a CRC32C, so a torn write is detected rather \
             than read back as missing records.",
        )
    } else {
        Measure::new(
            "checksums",
            "Per-group checksums",
            "no",
            Verdict::Act,
            "a section written before the format appended a checksum trailer",
            "This section predates the checksum trailer, so a truncated group would read back as \
             fewer records rather than as an error. The next flush writes the trailers: write to \
             the file, or flush the database.",
        )
    });

    // Skew: the failure mode that actually costs, and the one two extremes in
    // bytes could never show.
    let skew_threshold = format!(
        "watch above {}x the mean group, act above {}x; not judged below {} records per group",
        ratio(t::SKEW_WATCH),
        ratio(t::SKEW_ACT),
        ratio(t::SKEW_MIN_MEAN),
    );
    let share = stats.largest_group_share;
    measures.push(if groups.mean < t::SKEW_MIN_MEAN {
        Measure::new(
            "skew",
            "Group skew",
            if groups.mean > 0.0 {
                ratio(stats.skew)
            } else {
                "—".to_string()
            },
            Verdict::Good,
            &skew_threshold,
            format!(
                "Too few records per group ({} on average) for skew to mean anything yet.",
                ratio(groups.mean)
            ),
        )
    } else if stats.skew >= t::SKEW_ACT {
        Measure::new(
            "skew",
            "Group skew",
            format!("{}x", ratio(stats.skew)),
            Verdict::Act,
            &skew_threshold,
            format!(
                "The largest group holds {} records against a mean of {} - {} of the whole file. \
                 Every write landing there rewrites all of it, which is the one cost this format \
                 exists to avoid. Keys that share a prefix the hash does not separate are the usual \
                 cause.",
                groups.max,
                ratio(groups.mean),
                percent(share),
            ),
        )
    } else if stats.skew >= t::SKEW_WATCH {
        Measure::new(
            "skew",
            "Group skew",
            format!("{}x", ratio(stats.skew)),
            Verdict::Watch,
            &skew_threshold,
            format!(
                "The largest group holds {} records against a mean of {} ({} of the file). Writes \
                 to it cost more than writes elsewhere; worth watching as the file grows.",
                groups.max,
                ratio(groups.mean),
                percent(share),
            ),
        )
    } else {
        Measure::new(
            "skew",
            "Group skew",
            format!("{}x", ratio(stats.skew)),
            Verdict::Good,
            &skew_threshold,
            format!(
                "Records are spread evenly: the largest group holds {} against a mean of {}.",
                groups.max,
                ratio(groups.mean)
            ),
        )
    });

    // A count of mild outliers, which says something one extreme cannot: that
    // the hash itself is not spreading rather than that one group is unlucky.
    let overweight_threshold = format!(
        "watch when more than a tenth of the groups hold over {}x the mean",
        ratio(t::OVERWEIGHT_FACTOR)
    );
    let noisy = groups.mean >= t::SKEW_MIN_MEAN && groups.overweight * 10 > groups.groups;
    measures.push(Measure::new(
        "overweight_groups",
        "Overweight groups",
        format!("{} of {}", groups.overweight, groups.groups),
        if noisy { Verdict::Watch } else { Verdict::Good },
        &overweight_threshold,
        if noisy {
            format!(
                "{} of {} groups hold more than {}x the mean. One outlier is luck; this many says \
                 the keys are not spreading over the modulus.",
                groups.overweight,
                groups.groups,
                ratio(t::OVERWEIGHT_FACTOR)
            )
        } else if groups.mean < t::SKEW_MIN_MEAN {
            "Too few records per group to judge.".to_string()
        } else {
            format!(
                "{} of {} groups hold more than {}x the mean, which is within what an even hash \
                 looks like.",
                groups.overweight,
                groups.groups,
                ratio(t::OVERWEIGHT_FACTOR)
            )
        },
    ));

    // Headroom: how far the file is from the full rewrite a modulus change is.
    let load_threshold = format!(
        "watch above {} of the modulus' capacity, or below {} once it has grown",
        percent(t::LOAD_FACTOR_WATCH),
        percent(t::LOAD_FACTOR_SPARSE),
    );
    let headroom = match stats.records_until_shrink {
        Some(0) => "The next flush already picks a smaller modulus.".to_string(),
        Some(records) => format!("{} fewer records and the modulus halves.", records),
        None => "The modulus is already at its floor and will not shrink.".to_string(),
    };
    measures.push(if stats.load_factor >= t::LOAD_FACTOR_WATCH {
        Measure::new(
            "load_factor",
            "Load factor",
            percent(stats.load_factor),
            Verdict::Watch,
            &load_threshold,
            format!(
                "{} records over a modulus of {} at {} per group. {} more records and the modulus \
                 doubles, which rewrites every group in one flush. Normal and amortised - worth \
                 knowing before it happens on a busy file.",
                stats.record_count, stats.modulus, stats.records_per_group_target, stats.records_until_growth,
            ),
        )
    } else if stats.load_factor < t::LOAD_FACTOR_SPARSE && stats.records_until_shrink.is_some() {
        Measure::new(
            "load_factor",
            "Load factor",
            percent(stats.load_factor),
            Verdict::Watch,
            &load_threshold,
            format!(
                "The modulus of {} is far larger than {} records need, so the file is a directory of \
                 near-empty groups. It shrinks back on a flush. {}",
                stats.modulus, stats.record_count, headroom,
            ),
        )
    } else {
        Measure::new(
            "load_factor",
            "Load factor",
            percent(stats.load_factor),
            Verdict::Good,
            &load_threshold,
            format!(
                "{} records over a modulus of {}. {} more records before the modulus doubles. {}",
                stats.record_count, stats.modulus, stats.records_until_growth, headroom,
            ),
        )
    });

    // What the bytes are spent on. A group is rewritten whole from its live
    // entries, so a deleted record leaves no dead space behind inside one -
    // everything above the records themselves is the dictionary, the indexes
    // and the metadata.
    let other = stats
        .disk_bytes
        .saturating_sub(stats.group_bytes)
        .saturating_sub(stats.index_bytes);
    let per_record = if stats.record_count == 0 {
        "—".to_string()
    } else {
        bytes(stats.disk_bytes / stats.record_count.max(1))
    };
    measures.push(Measure::new(
        "space",
        "Bytes per record",
        per_record,
        Verdict::Good,
        "informational: a rewritten group is compacted, so records leave no dead space behind",
        format!(
            "{} on disk: {} of record groups, {} of indexes, {} of dictionary and metadata.",
            bytes(stats.disk_bytes),
            bytes(stats.group_bytes),
            bytes(stats.index_bytes),
            bytes(other),
        ),
    ));

    if groups.unreadable > 0 {
        measures.push(Measure::new(
            "distribution",
            "Groups counted",
            format!("{} of {}", groups.groups - groups.unreadable, groups.groups),
            Verdict::Watch,
            "watch when any group has no trailer to read a record count from",
            format!(
                "{} groups predate the checksum trailer, so their record counts could not be read \
                 without loading them and are not in the distribution above. The next flush writes \
                 the trailers.",
                groups.unreadable
            ),
        ));
    }

    // The indexes' own verdicts, rolled up. This is what makes an unhealthy
    // index findable from the file, and from the account listing above that,
    // rather than only by opening the index table.
    if !stats.indexes.is_empty() {
        let worst = stats
            .indexes
            .iter()
            .map(|index| index.health.verdict)
            .fold(Verdict::Good, Verdict::worse);
        let named: Vec<&str> = stats
            .indexes
            .iter()
            .filter(|index| index.health.verdict == worst)
            .map(|index| index.field.as_str())
            .collect();
        measures.push(Measure::new(
            "indexes",
            "Indexes",
            format!("{}", stats.indexes.len()),
            worst,
            "the worst verdict among this file's indexes",
            match worst {
                Verdict::Good => format!("All {} indexes are earning their keep.", stats.indexes.len()),
                _ => format!(
                    "{} of {} indexes need attention: {}. See the index table for what and why.",
                    named.len(),
                    stats.indexes.len(),
                    named.join(", "),
                ),
            },
        ));
    }

    Health::of(measures)
}
