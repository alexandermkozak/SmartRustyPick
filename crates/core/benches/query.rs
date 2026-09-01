use criterion::{Criterion, Throughput, black_box, criterion_group, criterion_main};
use smart_rusty_pick_core::db::Database;
use smart_rusty_pick_core::db::models::{ExplodeSpec, Record, SortSpec};

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
    let explode = ExplodeSpec {
        field_name: "ROLES".to_string(),
        condition: None,
    };
    let table_handle = db.get_table_read_only_for_account(common::ACCOUNT, TABLE).unwrap();
    let table = &*table_handle.read();

    // A bare explode gives every value a row; the criterion matches one
    // position in `MV_VALUES - 1` records per hundred.
    let selective_rows = (RECORDS / 100) * (common::MV_VALUES - 1);
    let shapes: [(&str, Option<&_>, Option<&ExplodeSpec>, usize); 3] = [
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
    let explode_idx = Database::explode_field_index(table, Some(&explode));
    let specs = [SortSpec {
        field_name: "ROLES".to_string(),
        descending: false,
    }];
    group.bench_function("sort_entries_in/by_exploded_value", |b| {
        b.iter_batched_ref(
            || base.clone(),
            |rows| {
                Database::sort_entries_in(table, rows, black_box(&specs), explode_idx);
                black_box(rows.len())
            },
            criterion::BatchSize::LargeInput,
        )
    });
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

criterion_group!(benches, bench_query, bench_sort, bench_explode, bench_serialize);
criterion_main!(benches);
