//! File and index health: the derived measures, and the promises about how
//! they are arrived at.
//!
//! Two of those promises are worth asserting rather than describing. The first
//! is that opening this view reads no record - `docs/web_dashboard.md` says the
//! dashboard never returns one, and the records-per-group figures would be the
//! easy way to break that. The second is that the thresholds live in one place:
//! a verdict is decided by the server, so the CLI, the protocol and the browser
//! cannot disagree about where the line is.

use crate::db::engine::Database;
use crate::db::hashfile;
use crate::db::health::{self, Verdict, thresholds};
use crate::db::models::*;
use crate::test_support::{TempDir, isolated_config};

const ACCOUNT: &str = "HEALTH";

fn open_account(base: &str) -> Database {
    let db = Database::new(base, Some(isolated_config())).unwrap();
    let _ = db.create_account(ACCOUNT, Some(base));
    db.logto(ACCOUNT).unwrap();
    db
}

/// `count` records with keys that hash evenly, and a `CITY` field to index.
fn build_file(db: &Database, file: &str, count: usize) {
    db.create_table(file).unwrap();
    let handle = db.get_table_mut(file).unwrap();
    {
        let mut table = handle.write();
        for (name, position) in [("NAME", 1), ("CITY", 2)] {
            table.dictionary.insert(
                name.to_string(),
                Record::from_display_string(&format!("{}^{}^L^20", position, name)),
            );
        }
        table.mark_dict_dirty();
        for i in 0..count {
            table.insert_record(
                &format!("K{:05}", i),
                Record::from_display_string(&format!("NAME{}^CITY{}", i, i % 10)),
            );
        }
    }
    db.save().unwrap();
}

fn measure<'a>(health: &'a crate::db::Health, id: &str) -> &'a crate::db::Measure {
    health
        .measures
        .iter()
        .find(|measure| measure.id == id)
        .unwrap_or_else(|| panic!("no `{}` measure; there are {:?}", id, ids(health)))
}

fn ids(health: &crate::db::Health) -> Vec<&str> {
    health.measures.iter().map(|measure| measure.id.as_str()).collect()
}

/// Files in the account whose cheap verdict is not `good`.
fn healthy_files(db: &Database) -> usize {
    db.file_health_for_account(ACCOUNT)
        .into_iter()
        .filter(|(_, summary)| summary.verdict != Verdict::Good)
        .count()
}

#[test]
fn records_per_group_comes_from_the_trailers_and_adds_up() {
    let guard = TempDir::new("health_distribution");
    let db = open_account(guard.path());
    build_file(&db, "PEOPLE", 500);

    let stats = db.file_statistics(ACCOUNT, "PEOPLE").unwrap();
    let groups = &stats.group_records;

    // Every group's count was readable, and they add up to the file. If the
    // trailers were being misread this is what would drift.
    assert_eq!(groups.unreadable, 0);
    let total = (groups.mean * groups.groups as f64).round() as u64;
    assert_eq!(
        total, stats.record_count,
        "the per-group counts do not add up to the file"
    );
    // Over the modulus, not the group files: a group holding nothing has no
    // file, and averaging only the files that exist would report every file as
    // perfectly even.
    assert_eq!(groups.groups as u64, stats.modulus);
    assert!(groups.min <= groups.median && groups.median <= groups.max);
    assert!(groups.max >= 1);

    // The buckets cover every group exactly once, which is what makes the
    // drawing of them honest.
    assert_eq!(
        groups.buckets.iter().map(|bucket| bucket.groups).sum::<usize>(),
        groups.groups - groups.unreadable
    );
    assert!(groups.buckets.len() <= thresholds::DISTRIBUTION_BUCKETS);
    assert_eq!(groups.buckets.first().unwrap().min, groups.min);
    assert_eq!(groups.buckets.last().unwrap().max, groups.max);

    // Skew is scale-free, so it is readable on a file of any size, and 500 keys
    // over the modulus the format chose should be nowhere near a warning.
    assert!(stats.skew < thresholds::SKEW_WATCH, "skew was {}", stats.skew);
    assert_eq!(measure(&stats.health, "skew").verdict, Verdict::Good);
}

#[test]
fn opening_the_statistics_view_loads_no_record() {
    // The property `docs/web_dashboard.md` promises. Records per group is the
    // easy way to break it - the count is in each group's trailer, and reading
    // the frames instead would be both slower and a lie about this view.
    let guard = TempDir::new("health_no_records");
    {
        let db = open_account(guard.path());
        build_file(&db, "PEOPLE", 300);
        db.create_index_for_account(ACCOUNT, "PEOPLE", "CITY").unwrap();
    }

    let db = open_account(guard.path());
    db.clear_loaded_tables();
    assert!(!db.is_table_loaded("PEOPLE"), "the fixture has to start cold");

    let stats = db.file_statistics(ACCOUNT, "PEOPLE").unwrap();
    assert_eq!(stats.record_count, 300);
    assert!(stats.group_records.max > 0, "the distribution was read");
    assert!(!stats.indexes.is_empty(), "the index was described");
    assert!(
        !db.is_table_loaded("PEOPLE"),
        "answering FILE.STATS loaded the file's records"
    );

    // Same for the account listing and the index report, which are the two
    // other places a roll-up could quietly open every file it walks.
    db.account_statistics();
    db.index_report(ACCOUNT, "PEOPLE", "CITY", 10).unwrap();
    assert!(!db.is_table_loaded("PEOPLE"), "a roll-up loaded the file's records");
}

#[test]
fn a_skewed_file_says_so_and_an_even_one_does_not() {
    let guard = TempDir::new("health_skew");
    let db = open_account(guard.path());

    // Keys chosen so that only one residue class is ever used: FNV-1a over a
    // given key is fixed, so filtering on it concentrates the file into a
    // fraction of the groups whatever modulus the flush then picks.
    db.create_table("LUMPY").unwrap();
    let handle = db.get_table_mut("LUMPY").unwrap();
    {
        let mut table = handle.write();
        let modulus = hashfile::MIN_MODULUS;
        let mut placed = 0;
        let mut i = 0u64;
        // Everything into the groups congruent to 0 mod 8, leaving most of the
        // modulus the flush chooses empty.
        while placed < 400 {
            let key = format!("K{:06}", i);
            if hashfile::group_of(&key, modulus) == 0 {
                table.insert_record(&key, Record::from_display_string("X"));
                placed += 1;
            }
            i += 1;
        }
    }
    db.save().unwrap();

    let stats = db.file_statistics(ACCOUNT, "LUMPY").unwrap();
    let skew = measure(&stats.health, "skew");
    assert_eq!(skew.verdict, Verdict::Act, "a piled-up file is not reported as skewed");
    // Most of the modulus holds nothing. A group with no records has no file at
    // all, so a distribution over the group *files* would have averaged the
    // four full ones and called this even.
    assert!(stats.group_count < stats.modulus as usize);
    assert!(stats.group_records.empty > stats.group_records.groups / 2);
    // The extremes say the same thing: the largest group is many times its fair
    // share of 1/modulus.
    assert!(
        stats.largest_group_share > 4.0 / stats.modulus as f64,
        "largest group is {} of the file over {} groups",
        stats.largest_group_share,
        stats.modulus
    );
    assert_eq!(stats.health.verdict, Verdict::Act);
    // The verdict comes with the rule that produced it, so a reader can argue
    // with the number rather than only with the word.
    assert!(skew.threshold.contains("mean group"));
    assert!(!skew.detail.is_empty());

    // The even file next door is not dragged into it.
    build_file(&db, "EVEN", 400);
    let even = db.file_statistics(ACCOUNT, "EVEN").unwrap();
    assert_eq!(measure(&even.health, "skew").verdict, Verdict::Good);
}

#[test]
fn a_file_too_small_to_judge_is_not_given_a_verdict_it_cannot_support() {
    // On four records over eight groups the largest group is three times the
    // mean, and that is simply what small numbers look like. Inventing a
    // verdict from too little data is how a dashboard teaches people to ignore
    // it.
    let guard = TempDir::new("health_small");
    let db = open_account(guard.path());
    build_file(&db, "TINY", 4);

    let stats = db.file_statistics(ACCOUNT, "TINY").unwrap();
    assert!(stats.group_records.mean < thresholds::SKEW_MIN_MEAN);
    assert_eq!(measure(&stats.health, "skew").verdict, Verdict::Good);
    assert!(measure(&stats.health, "skew").detail.contains("Too few records"));
    assert_eq!(measure(&stats.health, "overweight_groups").verdict, Verdict::Good);
}

#[test]
fn headroom_says_how_far_the_next_full_rewrite_is() {
    let guard = TempDir::new("health_headroom");
    let db = open_account(guard.path());
    build_file(&db, "PEOPLE", 100);

    let stats = db.file_statistics(ACCOUNT, "PEOPLE").unwrap();
    let capacity = stats.modulus * stats.records_per_group_target;
    assert!(capacity >= stats.record_count);
    assert_eq!(stats.records_until_growth, capacity + 1 - stats.record_count);
    assert!((stats.load_factor - stats.record_count as f64 / capacity as f64).abs() < 1e-9);

    // The figure is a prediction, so it is checked against the thing it
    // predicts rather than against itself: one more record than it names has to
    // be what actually moves the modulus.
    let per_group = stats.records_per_group_target as usize;
    let at_the_line = stats.record_count + stats.records_until_growth - 1;
    assert_eq!(
        hashfile::target_modulus(at_the_line, per_group),
        stats.modulus,
        "the modulus grew a record early"
    );
    assert!(
        hashfile::target_modulus(at_the_line + 1, per_group) > stats.modulus,
        "the modulus did not grow when the headroom ran out"
    );

    // And the same for the shrink, which is asymmetric on purpose so a file
    // hovering around a boundary does not rehash on every flush.
    match stats.records_until_shrink {
        None => assert_eq!(stats.modulus, hashfile::MIN_MODULUS),
        Some(records) => {
            let after = stats.record_count - records;
            assert!(hashfile::plan_modulus(stats.modulus, after, per_group) < stats.modulus);
            assert_eq!(
                hashfile::plan_modulus(stats.modulus, after + 1, per_group),
                stats.modulus
            );
        }
    }
}

#[test]
fn a_missing_checksum_is_a_warning_with_a_remedy_rather_than_a_neutral_row() {
    let guard = TempDir::new("health_integrity");
    let db = open_account(guard.path());
    build_file(&db, "PEOPLE", 20);

    let stats = db.file_statistics(ACCOUNT, "PEOPLE").unwrap();
    assert!(stats.checksums && !stats.legacy);
    assert_eq!(measure(&stats.health, "checksums").verdict, Verdict::Good);
    assert_eq!(measure(&stats.health, "format").verdict, Verdict::Good);

    // What the same two rows say when they are not satisfied. Judged from the
    // statistics rather than from a file doctored on disk, so this stays a test
    // of the rule and not of how a section is damaged.
    let damaged = FileStats {
        legacy: true,
        checksums: false,
        ..stats.clone()
    };
    let health = health::file_health(&damaged);
    assert_eq!(health.verdict, Verdict::Act);
    for id in ["format", "checksums"] {
        let row = measure(&health, id);
        assert_eq!(row.verdict, Verdict::Act, "`{}` is still a neutral row", id);
        assert!(
            row.detail.contains("next flush"),
            "`{}` says what is wrong without saying what fixes it",
            id
        );
    }
}

#[test]
fn a_files_verdict_absorbs_its_indexes() {
    // What connects the two views: a badly shaped index makes its file worth
    // opening, and the account listing above that says the account is.
    let guard = TempDir::new("health_rollup");
    let db = open_account(guard.path());
    build_file(&db, "PEOPLE", 200);

    let before = db.file_statistics(ACCOUNT, "PEOPLE").unwrap();
    assert_eq!(before.health.verdict, Verdict::Good);

    // CITY cycles over ten values, so no single value dominates - but nothing
    // has queried it, which is its own signal.
    db.create_index_for_account(ACCOUNT, "PEOPLE", "CITY").unwrap();
    let after = db.file_statistics(ACCOUNT, "PEOPLE").unwrap();
    let indexes = measure(&after.health, "indexes");
    assert_eq!(indexes.verdict, Verdict::Watch);
    assert!(indexes.detail.contains("CITY"), "the file does not name which index");
    assert_eq!(after.health.verdict, Verdict::Watch);
}

#[test]
fn the_listing_verdict_is_cheap_and_finds_a_stale_index() {
    let guard = TempDir::new("health_listing");
    let db = open_account(guard.path());
    build_file(&db, "PEOPLE", 60);
    build_file(&db, "OTHER", 60);
    db.create_index_for_account(ACCOUNT, "PEOPLE", "CITY").unwrap();

    assert_eq!(db.file_health_summary(ACCOUNT, "PEOPLE").verdict, Verdict::Good);

    // A `state` naming a data version that is not the file's is exactly what a
    // crash between the two flushes leaves behind, and is the thing the listing
    // exists to surface without opening anything.
    let section = crate::db::index::section_path(&format!("{}/PEOPLE", guard.path()), "CITY");
    let mut state = crate::db::index::read_state(&section).unwrap();
    state.data_version += 99;
    crate::db::index::write_state(&section, &state, hashfile::FsyncPolicy::Never).unwrap();

    let summary = db.file_health_summary(ACCOUNT, "PEOPLE");
    assert_eq!(summary.verdict, Verdict::Act);
    assert!(summary.reasons.iter().any(|reason| reason.contains("stale")));
    assert_eq!(db.file_health_summary(ACCOUNT, "OTHER").verdict, Verdict::Good);

    // And it rolls up to the account, so the problem is findable one level
    // further out again.
    let before = healthy_files(&db);
    let accounts = db.account_statistics();
    let account = accounts.iter().find(|stats| stats.name == ACCOUNT).unwrap();
    assert_eq!(account.index_count, 1);
    assert_eq!(account.stale_indexes, 1);
    assert_eq!(account.health.verdict, Verdict::Act);
    assert!(
        account.unhealthy_files >= 1 && account.unhealthy_files == before,
        "the account roll-up and the per-file check disagree"
    );
}

#[test]
fn a_verdict_is_the_worst_of_its_measures_and_nothing_else() {
    use crate::db::health::{Health, Measure};
    let good = Measure::new("a", "A", "1", Verdict::Good, "t", "d");
    let watch = Measure::new("b", "B", "2", Verdict::Watch, "t", "d");
    let act = Measure::new("c", "C", "3", Verdict::Act, "t", "d");

    assert_eq!(Health::of(vec![]).verdict, Verdict::Good);
    assert_eq!(Health::of(vec![good.clone()]).verdict, Verdict::Good);
    assert_eq!(Health::of(vec![good.clone(), watch.clone()]).verdict, Verdict::Watch);
    assert_eq!(
        Health::of(vec![watch.clone(), act.clone(), good.clone()]).verdict,
        Verdict::Act
    );

    // The concerns are worst first, which is the order a summary reads them in,
    // and the good ones are not concerns.
    let health = Health::of(vec![good, watch, act]);
    assert_eq!(
        health.concerns().iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
        vec!["c", "b"]
    );
}

#[test]
fn a_verdict_is_sent_as_the_string_a_client_branches_on() {
    // The wording of a detail may change; the verdict may not. Same rule as an
    // error code and its message.
    for (verdict, wire) in [
        (Verdict::Good, "good"),
        (Verdict::Watch, "watch"),
        (Verdict::Act, "act"),
    ] {
        assert_eq!(serde_json::to_string(&verdict).unwrap(), format!("\"{}\"", wire));
        assert_eq!(verdict.as_str(), wire);
    }
    assert_eq!(Verdict::Good.worse(Verdict::Act), Verdict::Act);
    assert_eq!(Verdict::Act.worse(Verdict::Watch), Verdict::Act);
}

#[test]
fn the_formatters_say_the_same_thing_the_dashboard_says() {
    assert_eq!(health::percent(0.912), "91%");
    assert_eq!(health::percent(0.0), "0%");
    assert_eq!(health::ratio(3.0), "3");
    assert_eq!(health::ratio(3.44), "3.4");
    assert_eq!(health::bytes(512), "512 B");
    assert_eq!(health::bytes(2048), "2.0 KB");
    // A ratio over nothing has no value rather than an infinite one.
    assert_eq!(health::ratio(f64::INFINITY), "—");
    assert_eq!(health::percent(f64::NAN), "—");
}
