//! Matcher axis: `run_match` wall-time vs signal count, swept independently of
//! node count (fixed at the ~100K basis). `run_match` needs `&mut Design`, so a
//! fresh design is rebuilt per iteration in the batched setup (excluded from the
//! timed region).

use criterion::{black_box, criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion};
use scale_bench::{generate, synth_signals, SynthConfig};
use svxprobe_matcher::{run_match, MatchOptions};
use svxprobe_model::Design;

const SIGNAL_COUNTS: [usize; 3] = [1_000, 10_000, 100_000];

fn bench_matcher(c: &mut Criterion) {
    let synth = generate(&SynthConfig::mid());
    let doc = synth.doc.clone();
    // Only needed to satisfy `synth_signals`' signature; it reads paths off `synth`.
    let scratch = Design::from_document(doc.clone());
    let opts = MatchOptions {
        excluded_scopes: vec![],
        anchor: Some(vec!["top".to_string()]),
    };

    let mut g = c.benchmark_group("matcher");
    for count in SIGNAL_COUNTS {
        let signals = synth_signals(&synth, &scratch, count);
        g.bench_with_input(
            BenchmarkId::new("run_match", count),
            &signals,
            |b, signals| {
                b.iter_batched(
                    || Design::from_document(doc.clone()),
                    |mut d| black_box(run_match(&mut d, signals, &opts)),
                    BatchSize::LargeInput,
                );
            },
        );
    }
    g.finish();
}

criterion_group!(benches, bench_matcher);
criterion_main!(benches);
