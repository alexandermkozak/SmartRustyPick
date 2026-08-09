use criterion::{BatchSize, Criterion, Throughput, black_box, criterion_group, criterion_main};

mod common;

const RECORDS: usize = 5_000;
const TABLE: &str = "BENCHDATA";

fn bench_save(c: &mut Criterion) {
    let mut group = c.benchmark_group("storage");
    group.throughput(Throughput::Elements(RECORDS as u64));
    group.sample_size(20);

    group.bench_function("build_and_save", |b| {
        b.iter_batched(
            || common::TempDir::new("save"),
            |dir| {
                let mut db = common::new_db(dir.path());
                common::build_table(&mut db, TABLE, RECORDS);
                db.save().unwrap();
                black_box(dir)
            },
            BatchSize::PerIteration,
        )
    });

    // Load from disk with a fresh `Database`, so the in-process table cache is bypassed.
    let dir = common::TempDir::new("load");
    {
        let mut db = common::new_db(dir.path());
        common::build_table(&mut db, TABLE, RECORDS);
        db.save().unwrap();
    }

    group.bench_function("load_from_disk", |b| {
        b.iter(|| {
            let mut db = common::new_db(black_box(dir.path()));
            let table = db.get_table_mut_for_account(common::ACCOUNT, TABLE).unwrap();
            black_box(table.records.len())
        })
    });

    group.finish();
}

fn bench_serialize(c: &mut Criterion) {
    let dir = common::TempDir::new("serialize");
    let mut db = common::new_db(dir.path());
    common::build_table(&mut db, TABLE, 1);
    let record = common::sample_record(7);

    let mut group = c.benchmark_group("storage");
    group.throughput(Throughput::Elements(1));
    group.bench_function("serialize_record_for_account", |b| {
        b.iter(|| {
            black_box(db.serialize_record_for_account(
                black_box(common::ACCOUNT),
                black_box(TABLE),
                black_box(&record),
            ))
        })
    });
    group.finish();
}

criterion_group!(benches, bench_save, bench_serialize);
criterion_main!(benches);
