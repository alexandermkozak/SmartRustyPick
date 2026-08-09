use criterion::{Criterion, black_box, criterion_group, criterion_main};
use smart_rusty_pick_core::db::models::{Field, Record, Value};

mod common;

/// `NAME^CITY^AMOUNT`, the shape of a typical small record.
fn small_record() -> Record {
    common::sample_record(42)
}

/// ~50 fields, every third of them multi-valued with sub-values.
fn wide_record() -> Record {
    let mut rec = Record::new();
    for i in 0..50 {
        if i % 3 == 0 {
            rec.fields.push(Field {
                values: vec![
                    Value { sub_values: vec![format!("v{i}a"), format!("v{i}b")] },
                    Value { sub_values: vec![format!("v{i}c")] },
                ],
            });
        } else {
            rec.fields.push(common::field(&format!("field{i}value")));
        }
    }
    rec
}

fn bench_record_codec(c: &mut Criterion) {
    let cases = [("small", small_record()), ("wide", wide_record())];

    let mut group = c.benchmark_group("record_codec");
    for (name, record) in &cases {
        let bytes = record.to_bytes();
        let display = record.to_display_string();

        group.bench_function(format!("from_bytes/{name}"), |b| {
            b.iter(|| black_box(Record::from_bytes(black_box(&bytes))))
        });
        group.bench_function(format!("to_bytes/{name}"), |b| {
            b.iter(|| black_box(black_box(record).to_bytes()))
        });
        group.bench_function(format!("from_display_string/{name}"), |b| {
            b.iter(|| black_box(Record::from_display_string(black_box(&display))))
        });
        group.bench_function(format!("to_display_string/{name}"), |b| {
            b.iter(|| black_box(black_box(record).to_display_string()))
        });
        group.bench_function(format!("get_field_display_string/{name}"), |b| {
            b.iter(|| black_box(black_box(record).get_field_display_string(black_box(1))))
        });
    }
    group.finish();
}

criterion_group!(benches, bench_record_codec);
criterion_main!(benches);
