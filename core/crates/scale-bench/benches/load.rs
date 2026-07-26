//! Cold-load axis: the eager parse + validate + index-build path, plus its
//! parse-only and index-only decomposition, at 665 / 100K (/ 1M with
//! `SCALE_BENCH_FULL`). A `665_golden` row loads the real fixture to anchor the
//! synthetic baseline against reality.

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

        // cache_hit: write the JSON to a temp file, pre-build the rkyv archive,
        // then measure the warm load path (mmap → deserialize → index build) that
        // replaces `from_slice`'s JSON parse on repeat launches (#21).
        let tmp = std::env::temp_dir().join(format!("scale_bench_cache_{name}"));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let model_path = tmp.join("hierarchy.json");
        std::fs::write(&model_path, &json).unwrap();
        svxprobe_ingest::build_cache(&model_path).unwrap();
        g.bench_with_input(BenchmarkId::new("cache_hit", name), &model_path, |b, p| {
            b.iter(|| svxprobe_ingest::from_path(black_box(p)).unwrap());
        });
    }

    // Embedded at compile time (#240), one accessor shared with the scenario and
    // report paths — this row is the anchor against reality, so it must not go
    // missing just because a build ran from another directory.
    if let Some(bytes) = scale_bench::golden::golden_bytes() {
        g.bench_function("from_slice/665_golden", |b| {
            b.iter(|| svxprobe_ingest::from_slice(black_box(&bytes)).unwrap());
        });
    }
    g.finish();
}

criterion_group!(benches, bench_load);
criterion_main!(benches);
