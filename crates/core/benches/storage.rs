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

/// What an index costs the write path, and where that cost comes from.
///
/// The same single-record write as `incremental_write`, with one index on the
/// file. Two shapes of index, because they do not behave alike:
///
/// * `unique_field` indexes `NAME`, which is distinct per record. Each entry
///   holds one key, so the write stays flat as the file grows - the same
///   property the record write has.
/// * `shared_field` indexes `CITY`, which has ten values whatever the size of
///   the file. An entry then holds a tenth of the keys in the file, and
///   rewriting it costs proportionally more as the file grows.
///
/// That second row is the honest cost of an equality index on a field with few
/// values, and it is measured here rather than assumed.
fn bench_indexed_write(c: &mut Criterion) {
    let mut group = c.benchmark_group("storage");
    group.throughput(Throughput::Elements(1));

    for records in [1_000usize, 10_000usize] {
        for (label, field) in [("unique_field", "NAME"), ("shared_field", "CITY")] {
            let dir = common::TempDir::new(&format!("idxwrite{records}{label}"));
            let mut db = common::new_db(dir.path());
            common::build_table(&mut db, TABLE, records);
            db.save().unwrap();
            db.create_index_for_account(common::ACCOUNT, TABLE, field).unwrap();

            group.bench_function(format!("indexed_write/{label}/{records}_records"), |b| {
                let mut counter = 0usize;
                b.iter(|| {
                    counter += 1;
                    // Each write really moves a value - the record replacing the
                    // key carries a different NAME - while the set of values
                    // stays bounded, so the index reaches a steady size instead
                    // of growing for the length of the run.
                    let table_handle = db.get_table_mut_for_account(common::ACCOUNT, TABLE).unwrap();
                    let mut table = table_handle.write();
                    table.insert_record(
                        &format!("K{:06}", counter % records),
                        common::sample_record(counter % (2 * records)),
                    );
                    drop(table);
                    db.save().unwrap();
                    black_box(counter)
                })
            });
        }
    }

    group.finish();
}

/// What excluding the dominant value of a skewed field does to the write cost.
///
/// The motivating case for index exclusions, measured rather than argued. The
/// field is `STATUS`: `ACTIVE` on nine records in ten, and one of five hundred
/// rare values on the tenth.
///
/// * `whole` indexes every value. The `ACTIVE` posting list holds nine tenths
///   of the file's keys, so every write that touches a record carrying it
///   rewrites that entry - the longest one in the index.
/// * `excluding_dominant` indexes everything but `ACTIVE`. The rare values are
///   still found through the index; a query for `ACTIVE` falls back to the scan
///   that was going to do the work anyway.
///
/// The gap between the two rows is what the exclusion is worth, and it widens
/// with the file: the dominant entry grows with the records while the rare ones
/// do not.
fn bench_excluded_write(c: &mut Criterion) {
    let mut group = c.benchmark_group("storage");
    group.throughput(Throughput::Elements(1));

    for records in [1_000usize, 10_000usize] {
        for (label, exclude) in [("whole", &[][..]), ("excluding_dominant", &["ACTIVE".to_string()][..])] {
            let dir = common::TempDir::new(&format!("excl{records}{label}"));
            let mut db = common::new_db(dir.path());
            common::build_dominant_table(&mut db, TABLE, records);
            db.save().unwrap();
            db.create_index_excluding(common::ACCOUNT, TABLE, "STATUS", exclude)
                .unwrap();

            group.bench_function(format!("excluded_write/{label}/{records}_records"), |b| {
                let mut counter = 0usize;
                b.iter(|| {
                    counter += 1;
                    // One record, whose status alternates. The write has to
                    // *move* the indexed value or it costs the index nothing at
                    // all - re-storing a record with the value it already had
                    // compares two short lists and stops - and holding the key
                    // fixed keeps the data-group write constant, so the whole
                    // difference between the two rows is the index.
                    let status = if counter.is_multiple_of(2) { "ACTIVE" } else { "RARE7" };
                    let table_handle = db.get_table_mut_for_account(common::ACCOUNT, TABLE).unwrap();
                    let mut table = table_handle.write();
                    table.insert_record("K000007", common::record_with_status(7, status));
                    drop(table);
                    db.save().unwrap();
                    black_box(counter)
                })
            });
        }
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

criterion_group!(
    benches,
    bench_save,
    bench_incremental_write,
    bench_indexed_write,
    bench_excluded_write,
    bench_serialize
);
criterion_main!(benches);
