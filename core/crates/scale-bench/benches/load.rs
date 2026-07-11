//! Cold-load axis: the eager parse + validate + index-build path, plus its
//! parse-only and index-only decomposition, at 665 / 100K (/ 1M with
//! `SCALE_BENCH_FULL`). A `665_golden` row loads the real fixture to anchor the
//! synthetic baseline against reality.

use std::path::PathBuf;

use criterion::{black_box, criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion};
use scale_bench::{generate, to_json, SynthConfig};
use svxprobe_model::{Design, Document};

fn sizes() -> Vec<(&'static str, SynthConfig)> {
    let mut v = vec![
        ("665", SynthConfig::baseline()),
        ("100K", SynthConfig::mid()),
    ];
    if std::env::var_os("SCALE_BENCH_FULL").is_some() {
        v.push(("1M", SynthConfig::large()));
    }
    v
}

fn golden_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../fixtures/picorv32_soc/golden/hierarchy.json")
}

fn bench_load(c: &mut Criterion) {
    let mut g = c.benchmark_group("load");
    for (name, cfg) in sizes() {
        let synth = generate(&cfg);
        let json = to_json(&synth.doc);

        g.bench_with_input(BenchmarkId::new("from_slice", name), &json, |b, json| {
            b.iter(|| svxprobe_ingest::from_slice(black_box(json)).unwrap());
        });
        g.bench_with_input(BenchmarkId::new("parse_only", name), &json, |b, json| {
            b.iter(|| {
                let d: Document = serde_json::from_slice(black_box(json)).unwrap();
                black_box(d);
            });
        });
        let doc = synth.doc;
        g.bench_with_input(BenchmarkId::new("index_build", name), &doc, |b, doc| {
            b.iter_batched(
                || doc.clone(),
                |d| black_box(Design::from_document(d)),
                BatchSize::SmallInput,
            );
        });
    }

    if let Ok(bytes) = std::fs::read(golden_path()) {
        g.bench_function("from_slice/665_golden", |b| {
            b.iter(|| svxprobe_ingest::from_slice(black_box(&bytes)).unwrap());
        });
    }
    g.finish();
}

criterion_group!(benches, bench_load);
criterion_main!(benches);
