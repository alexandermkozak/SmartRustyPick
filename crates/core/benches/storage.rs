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
            let db = common::new_db(black_box(dir.path()));
            let table_handle = db.get_table_mut_for_account(common::ACCOUNT, TABLE).unwrap();
            let table = table_handle.write();
            black_box(table.records.len())
        })
    });

    group.finish();
}

/// The write path the remote server uses: change one record of an established
/// table and flush. With the hashed layout this rewrites a single group, so the
/// cost must not track the size of the table.
fn bench_incremental_write(c: &mut Criterion) {
    let mut group = c.benchmark_group("storage");
    group.throughput(Throughput::Elements(1));

    for records in [1_000usize, 10_000usize] {
        let dir = common::TempDir::new(&format!("incr{records}"));
        let mut db = common::new_db(dir.path());
        common::build_table(&mut db, TABLE, records);
        db.save().unwrap();

        group.bench_function(format!("incremental_write/{records}_records"), |b| {
            let mut counter = 0usize;
            b.iter(|| {
                counter += 1;
                let table_handle = db.get_table_mut_for_account(common::ACCOUNT, TABLE).unwrap();
                let mut table = table_handle.write();
                table.insert_record(&format!("K{:06}", counter % records), common::sample_record(counter));
                drop(table);
                db.save().unwrap();
                black_box(counter)
            })
        });
    }

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
            black_box(db.serialize_record_for_account(black_box(common::ACCOUNT), black_box(TABLE), black_box(&record)))
        })
    });
    group.finish();
}

criterion_group!(benches, bench_save, bench_incremental_write, bench_serialize);
criterion_main!(benches);
