//! Scoped-query + cone axes: `scope_graph`/`expand` (each currently a full
//! design-edge scan), the `nodes_at_path`/`nodes_at_source` index lookups behind
//! `probe_node`/`probe_source`, and `cone()` under the high-fan-out clock net
//! (the frontier-explosion case). 1M is gated behind `SCALE_BENCH_FULL`.

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use scale_bench::{build_design, SynthConfig};
use svxprobe_model::Dir;

const CONE_DEPTH: usize = 6;

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

fn bench_query(c: &mut Criterion) {
    let mut g = c.benchmark_group("query");
    for (name, cfg) in sizes() {
        let (design, synth) = build_design(&cfg);

        g.bench_with_input(BenchmarkId::new("scope_graph", name), &design, |b, d| {
            b.iter(|| black_box(svxprobe_schematic::scope_graph(d, &synth.mid_scope)));
        });
        g.bench_with_input(BenchmarkId::new("expand", name), &design, |b, d| {
            b.iter(|| black_box(svxprobe_schematic::expand(d, synth.mid_instance)));
        });
        g.bench_with_input(BenchmarkId::new("nodes_at_path", name), &design, |b, d| {
            b.iter(|| black_box(d.nodes_at_path(&synth.probe_path)));
        });
        let (file, off) = synth.probe_source;
        g.bench_with_input(
            BenchmarkId::new("nodes_at_source", name),
            &design,
            |b, d| {
                b.iter(|| black_box(d.nodes_at_source(file, off)));
            },
        );
        g.bench_with_input(BenchmarkId::new("cone_clock", name), &design, |b, d| {
            b.iter(|| {
                black_box(svxprobe_schematic::cone(
                    d,
                    synth.clock_net,
                    Dir::Out,
                    CONE_DEPTH,
                ))
            });
        });
    }
    g.finish();
}

criterion_group!(benches, bench_query);
criterion_main!(benches);
