use criterion::{criterion_group, criterion_main, Criterion};
use nickel_lang_utilities::{ncl_bench_group, EvalMode};
use pprof::criterion::{Output, PProfProfiler};

ncl_bench_group! {
    name = benches;
    config = Criterion::default().with_profiler(PProfProfiler::new(100, Output::Flamegraph(None)));
    {
        name = "church 3",
        path = "functions/church",
        args = (3),
    }
}
criterion_main!(benches);
