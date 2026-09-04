use criterion::{Criterion, Throughput, black_box, criterion_group, criterion_main};
use smart_rusty_pick_core::db::Database;
use smart_rusty_pick_core::db::models::{ExplodeTarget, Record, SortSpec};

mod common;

const RECORDS: usize = 10_000;
const TABLE: &str = "BENCHDATA";

fn bench_query(c: &mut Criterion) {
    let dir = common::TempDir::new("query");
    let mut db = common::new_db(dir.path());
    common::build_table(&mut db, TABLE, RECORDS);

    // Unique match, ~10% attribute match and a full scan.
    let shapes: [(&str, Vec<&str>, u64); 3] = [
        ("unique", vec!["WITH", "NAME", "=", "NAME4242"], 1),
        ("attribute", vec!["WITH", "CITY", "=", "CITY3"], (RECORDS / 10) as u64),
        ("full_scan", vec!["WITH", "NAME", "!=", "NOPE"], RECORDS as u64),
    ];

    let mut group = c.benchmark_group("query");
    group.throughput(Throughput::Elements(RECORDS as u64));

    for (name, parts, expected) in &shapes {
        group.bench_function(format!("parse_query/{name}"), |b| {
            b.iter(|| black_box(db.parse_query(TABLE, black_box(parts))))
        });

        let node = db.parse_query(TABLE, parts).unwrap();
        let hits = db.query_for_account(common::ACCOUNT, TABLE, false, &node, None).len();
        assert_eq!(hits as u64, *expected, "unexpected result count for {name}");

        group.bench_function(format!("query_for_account/{name}"), |b| {
            b.iter(|| {
                black_box(db.query_for_account(
                    black_box(common::ACCOUNT),
                    black_box(TABLE),
                    false,
                    black_box(&node),
                    None,
                ))
            })
        });
    }
    group.finish();
}

/// What a secondary index is for: the cost of finding a record by a non-key
/// value stops growing with the size of the file.
///
/// Shaped like `incremental_write` in `benches/storage.rs` - the same query at
/// two file sizes - because the number that matters is not the absolute time
/// but whether it moves when the file gets ten times bigger. The scan beside it
/// is the control, and it moves by exactly that factor.
fn bench_index(c: &mut Criterion) {
    let mut group = c.benchmark_group("index");
    group.throughput(Throughput::Elements(1));

    for records in [1_000usize, 10_000usize] {
        let dir = common::TempDir::new(&format!("index{records}"));
        let mut db = common::new_db(dir.path());
        common::build_table(&mut db, TABLE, records);
        db.save().unwrap();

        // NAME is distinct per record, which is the shape an index is for. The
        // same query runs both ways, so the two rows differ only in the plan.
        let node = db.parse_query(TABLE, &["WITH", "NAME", "=", "NAME42"]).unwrap();
        assert_eq!(
            db.query_for_account(common::ACCOUNT, TABLE, false, &node, None).len(),
            1
        );

        group.bench_function(format!("scan/{records}_records"), |b| {
            b.iter(|| {
                black_box(
                    db.query_for_account(black_box(common::ACCOUNT), black_box(TABLE), false, &node, None)
                        .len(),
                )
            })
        });

        db.create_index_for_account(common::ACCOUNT, TABLE, "NAME").unwrap();
        assert_eq!(
            db.query_for_account(common::ACCOUNT, TABLE, false, &node, None).len(),
            1
        );

        group.bench_function(format!("indexed/{records}_records"), |b| {
            b.iter(|| {
                black_box(
                    db.query_for_account(black_box(common::ACCOUNT), black_box(TABLE), false, &node, None)
                        .len(),
                )
            })
        });
    }

    group.finish();
}

fn bench_sort(c: &mut Criterion) {
    let dir = common::TempDir::new("sort");
    let mut db = common::new_db(dir.path());
    common::build_table(&mut db, TABLE, RECORDS);

    let base: Vec<(String, Record)> = (0..RECORDS)
        .map(|i| (format!("K{i:06}"), common::sample_record(i)))
        .collect();

    let specs = [
        (
            "by_name",
            vec![SortSpec {
                field_name: "NAME".to_string(),
                descending: false,
            }],
        ),
        (
            "by_id_desc",
            vec![SortSpec {
                field_name: "ID".to_string(),
                descending: true,
            }],
        ),
        (
            "by_city_then_amount",
            vec![
                SortSpec {
                    field_name: "CITY".to_string(),
                    descending: false,
                },
                SortSpec {
                    field_name: "AMOUNT".to_string(),
                    descending: true,
                },
            ],
        ),
    ];

    let mut group = c.benchmark_group("sort");
    group.throughput(Throughput::Elements(RECORDS as u64));
    for (name, spec) in &specs {
        group.bench_function(format!("sort_results_for_account/{name}"), |b| {
            b.iter_batched_ref(
                || base.clone(),
                |results| {
                    db.sort_results_for_account(common::ACCOUNT, TABLE, results, black_box(spec));
                    black_box(results.len())
                },
                criterion::BatchSize::LargeInput,
            )
        });
    }
    group.finish();
}

/// Exploding a query turns per-record work into per-value work, so these
/// measure what the existing `query` group cannot: a result set bounded by
/// values rather than by records.
///
/// `unexploded` is the same criterion with no explode clause, so the cost of
/// exploding reads as the delta between the two rather than as an absolute.
fn bench_explode(c: &mut Criterion) {
    let dir = common::TempDir::new("explode");
    let mut db = common::new_db(dir.path());
    common::build_mv_table(&mut db, TABLE, RECORDS);

    let node = db.parse_query(TABLE, &["WITH", "ROLES", "=", "ROLE42"]).unwrap();
    let table_handle = db.get_table_read_only_for_account(common::ACCOUNT, TABLE).unwrap();
    let table = &*table_handle.read();
    // `ROLES` is in no association, so this resolves to the lone-field target -
    // the shape the numbers below have always described.
    let explode = Database::explode_target_in(table, "ROLES").unwrap();

    // A bare explode gives every value a row; the criterion matches one
    // position in `MV_VALUES - 1` records per hundred.
    let selective_rows = (RECORDS / 100) * (common::MV_VALUES - 1);
    let shapes: [(&str, Option<&_>, Option<&ExplodeTarget>, usize); 3] = [
        ("bare", None, Some(&explode), RECORDS * common::MV_VALUES),
        ("selective", Some(&node), Some(&explode), selective_rows),
        ("unexploded", Some(&node), None, selective_rows),
    ];

    let mut group = c.benchmark_group("explode");
    group.throughput(Throughput::Elements(RECORDS as u64));
    for (name, query, spec, expected) in shapes {
        let rows = Database::query_exploded_in(table, false, query, spec, None).len();
        assert_eq!(rows, expected, "unexpected row count for {name}");

        group.bench_function(format!("query_exploded_in/{name}"), |b| {
            b.iter(|| {
                black_box(Database::query_exploded_in(
                    black_box(table),
                    false,
                    black_box(query),
                    black_box(spec),
                    None,
                ))
            })
        });
    }

    // Sorting an exploded result resolves the exploded column per row rather
    // than once per record, which is the one place the pre-resolved sort keys
    // do extra work.
    let base = Database::query_exploded_in(table, false, Some(&node), Some(&explode), None);
    let specs = [SortSpec {
        field_name: "ROLES".to_string(),
        descending: false,
    }];
    group.bench_function("sort_entries_in/by_exploded_value", |b| {
        b.iter_batched_ref(
            || base.clone(),
            |rows| {
                Database::sort_entries_in(table, rows, black_box(&specs), Some(&explode));
                black_box(rows.len())
            },
            criterion::BatchSize::LargeInput,
        )
    });
    group.finish();
}

/// `SELECT` keeps keys and throws the records away, so what it costs should
/// track the size of its answer, not the size of the records it read.
///
/// `owning_baseline` is the path the handler used to run: `query_for_account`,
/// which clones every match, then the sort, then the keys taken off the clones.
/// It is the number the keys-only path of the same shape is read against.
/// `unsorted` never looks at a record at all; `sorted` borrows one per match to
/// read the sort column from.
///
/// Measured over two record shapes on the same selective criterion, because
/// the clone is what scales with the record and the scan is not: `narrow` is
/// three single-valued fields, `wide` adds a field of eight values, so the gap
/// between the baseline and the keys-only paths widens with the record while
/// the work of finding the matches stays put.
fn bench_select(c: &mut Criterion) {
    let dir = common::TempDir::new("select");
    let mut db = common::new_db(dir.path());
    common::build_table(&mut db, "NARROW", RECORDS);
    common::build_mv_table(&mut db, "WIDE", RECORDS);

    let sort_specs = [SortSpec {
        field_name: "NAME".to_string(),
        descending: false,
    }];
    let shapes: [(&str, &[SortSpec]); 2] = [("unsorted", &[]), ("sorted", &sort_specs)];

    let mut group = c.benchmark_group("select");
    group.throughput(Throughput::Elements(RECORDS as u64));

    for width in ["narrow", "wide"] {
        let table_name = if width == "narrow" { "NARROW" } else { "WIDE" };
        // Selective: one record in ten, out of a file of ten thousand.
        let node = db.parse_query(table_name, &["WITH", "CITY", "=", "CITY3"]).unwrap();
        let expected = RECORDS / 10;

        for (name, specs) in shapes {
            group.bench_function(format!("owning_baseline/{width}/{name}"), |b| {
                b.iter(|| {
                    let mut results =
                        db.query_for_account(black_box(common::ACCOUNT), black_box(table_name), false, &node, None);
                    db.sort_results_for_account(common::ACCOUNT, table_name, &mut results, black_box(specs));
                    black_box(results.into_iter().map(|(key, _)| key).collect::<Vec<String>>())
                })
            });
        }

        let table_handle = db.get_table_read_only_for_account(common::ACCOUNT, table_name).unwrap();
        let table = &*table_handle.read();
        for (name, specs) in shapes {
            let entries = Database::select_entries_in(table, false, Some(&node), None, None, specs);
            assert_eq!(entries.len(), expected, "unexpected row count for {width}/{name}");

            group.bench_function(format!("select_entries_in/{width}/{name}"), |b| {
                b.iter(|| {
                    black_box(Database::select_entries_in(
                        black_box(table),
                        false,
                        black_box(Some(&node)),
                        None,
                        None,
                        black_box(specs),
                    ))
                })
            });
        }
    }
    group.finish();
}

/// Record serialisation is on the READ, QUERY and GET.NEXT path for every
/// client, exploded or not, so `single_valued` is the guard that shaping
/// multivalues as JSON arrays did not cost the common case anything.
fn bench_serialize(c: &mut Criterion) {
    let dir = common::TempDir::new("serialize");
    let mut db = common::new_db(dir.path());
    common::build_table(&mut db, "PLAIN", RECORDS);
    common::build_mv_table(&mut db, "MULTI", RECORDS);

    let mut group = c.benchmark_group("serialize");
    group.throughput(Throughput::Elements(RECORDS as u64));
    for (name, table_name) in [("single_valued", "PLAIN"), ("multivalued", "MULTI")] {
        let table_handle = db.get_table_read_only_for_account(common::ACCOUNT, table_name).unwrap();
        let table = &*table_handle.read();
        let schema = db.record_schema(table);
        let records: Vec<&Record> = table.records.values().collect();

        group.bench_function(format!("serialize_record_with_schema/{name}"), |b| {
            b.iter(|| {
                for record in &records {
                    black_box(db.serialize_record_with_schema(black_box(&schema), record));
                }
            })
        });
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_query,
    bench_index,
    bench_sort,
    bench_explode,
    bench_select,
    bench_serialize
);
criterion_main!(benches);
