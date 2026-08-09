use criterion::{Criterion, Throughput, black_box, criterion_group, criterion_main};
use smart_rusty_pick_core::db::models::{Record, SortSpec};

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
        ("by_name", vec![SortSpec { field_name: "NAME".to_string(), descending: false }]),
        ("by_id_desc", vec![SortSpec { field_name: "ID".to_string(), descending: true }]),
        (
            "by_city_then_amount",
            vec![
                SortSpec { field_name: "CITY".to_string(), descending: false },
                SortSpec { field_name: "AMOUNT".to_string(), descending: true },
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

criterion_group!(benches, bench_query, bench_sort);
criterion_main!(benches);
