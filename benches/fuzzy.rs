mod shared;

use {
    broot::pattern::FuzzyPattern,
    criterion::{black_box, criterion_group, criterion_main, Criterion},
};

static PATTERNS: &[&str] = &["réveil", "AB", "e", "brt", "brootz"];

fn score_of_benchmark(c: &mut Criterion) {
    for pattern in PATTERNS {
        let task = format!("FuzzyPattern({:?})::score_of", pattern);
        c.bench_function(&task, |b| {
            let fp = FuzzyPattern::from(pattern);
            b.iter(|| {
                for name in shared::NAMES {
                    black_box(fp.score_of(name));
                }
            });
        });
    }
}

criterion_group!(
    name = fuzzy;
    config = Criterion::default().without_plots();
    targets = score_of_benchmark,
);
criterion_main!(fuzzy);
