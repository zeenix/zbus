use criterion::{black_box, criterion_group, criterion_main, Criterion};

fn name_parse(c: &mut Criterion) {
    const WELL_KNOWN_NAME: &'static str = "a.very.loooooooooooooooooo-ooooooo_0000o0ng.Name.\
            That.Is.Valid.For.DBus.and_good.For.benchmarks.I-guess";
    const UNIQUE_NAME: &'static str = ":a.very.loooooooooooooooooo-ooooooo_0000o0ng.Name.\
            That.Is.Valid.For.DBus.and_good.For.benchmarks.I-guess";

    let mut group = c.benchmark_group("parse_name");
    group.sample_size(1000);

    group.bench_function("well_known", |b| {
        b.iter(|| {
            zbus_names::WellKnownName::try_from(black_box(WELL_KNOWN_NAME)).unwrap();
        })
    });

    group.bench_function("unique", |b| {
        b.iter(|| {
            zbus_names::UniqueName::try_from(black_box(UNIQUE_NAME)).unwrap();
        })
    });

    group.bench_function("bus", |b| {
        b.iter(|| {
            // Use a well-known name since the parser first tries unique name.
            zbus_names::BusName::try_from(black_box(WELL_KNOWN_NAME)).unwrap();
        })
    });

    group.finish();
}

criterion_group!(benches, name_parse);
criterion_main!(benches);
