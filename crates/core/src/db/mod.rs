#[cfg(test)]
mod db_tests;
#[cfg(test)]
mod durability_tests;
pub mod engine;
#[cfg(test)]
mod engine_tests;
pub mod error;
pub mod hashfile;
#[cfg(test)]
mod hashfile_tests;
pub mod health;
#[cfg(test)]
mod health_tests;
pub mod index;
#[cfg(test)]
mod index_tests;
#[cfg(test)]
mod model_tests;
pub mod models;
pub mod query;
#[cfg(test)]
mod query_tests;
pub mod report;
#[cfg(test)]
mod report_tests;

pub use engine::{Database, TableHandle, TableKey};
pub use error::{DbError, DbResult};
pub use health::{Health, HealthSummary, Measure, Verdict};
pub use index::{FileIndex, IndexReport, IndexStats, IndexUsageStats, IndexValue};
pub use models::*;
