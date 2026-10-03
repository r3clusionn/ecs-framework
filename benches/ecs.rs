//! tessera against hecs on the usual ECS workloads (after ecs_bench_suite), timed with benchlab.
//!
//!     cargo bench --bench ecs [-- report.json]

use benchlab::report::{fmt_env, render_table};
use benchlab::{Config, Suite, Throughput};
use tessera::system::Query;
use tessera::{Schedule, World};

#[derive(Clone, Copy)]
struct Transform([f32; 16]);
#[derive(Clone, Copy)]
struct Position([f32; 3]);
#[derive(Clone, Copy)]
struct Rotation(#[allow(dead_code)] [f32; 3]);
#[derive(Clone, Copy)]
struct Velocity([f32; 3]);
#[derive(Clone, Copy)]
struct A(f32);
#[derive(Clone, Copy)]
struct B(f32);
#[derive(Clone, Copy)]
struct C(f32);
#[derive(Clone, Copy)]
struct D(f32);
#[derive(Clone, Copy)]
struct E(f32);

fn bundle() -> (Transform, Position, Rotation, Velocity) {
    (Transform([1.0; 16]), Position([1.0; 3]), Rotation([1.0; 3]), Velocity([1.0; 3]))
}

macro_rules! frag {
    ($w:expr, $spawn:ident, $($n:ident),*) => {
        $(
            #[derive(Clone, Copy)]
            struct $n(#[allow(dead_code)] f32);
            for _ in 0..20 {
                $w.$spawn(($n(0.0), Data(1.0)));
            }
        )*
    };
}

#[derive(Clone, Copy)]
struct Data(f32);

/// A 4x4 matrix inverse by cofactors, repeated: the "heavy compute" workload.
fn heavy(m: &mut [f32; 16]) {
    for _ in 0..100 {
        let a = *m;
        let det = a[0] * (a[5] * a[10] - a[6] * a[9]) - a[1] * (a[4] * a[10] - a[6] * a[8]) + a[2] * (a[4] * a[9] - a[5] * a[8]);
        let inv = if det.abs() > 1e-6 { 1.0 / det } else { 1.0 };
        for (i, v) in m.iter_mut().enumerate() {
            *v = a[(i * 7 + 3) % 16] * inv + a[i] * 0.5;
        }
    }
}

fn main() {
    // Pinned to one core: on a CPU with performance and efficiency cores an unpinned run can land
    // on either, which changes results by 2x. BENCH_CPU picks another core.
    let cpu = std::env::var("BENCH_CPU").ok().and_then(|c| c.parse().ok()).unwrap_or(2);
    let mut suite = Suite::new(Config { samples: 30, ..Config::default() }).pin(cpu).expect("pin");
    let n = Throughput::Elements(10_000);

    suite.bench_throughput("simple_insert 10k: tessera", n, || {
        let mut w = World::new();
        for _ in 0..10_000 {
            w.spawn(bundle());
        }
        w
    });
    suite.bench_throughput("simple_insert 10k: tessera spawn_batch", n, || {
        let mut w = World::new();
        w.spawn_batch((0..10_000).map(|_| bundle()));
        w
    });
    suite.bench_throughput("simple_insert 10k: hecs", n, || {
        let mut w = hecs::World::new();
        for _ in 0..10_000 {
            w.spawn(bundle());
        }
        w
    });
    suite.bench_throughput("simple_insert 10k: hecs spawn_batch", n, || {
        let mut w = hecs::World::new();
        w.spawn_batch((0..10_000).map(|_| bundle())).for_each(drop);
        w
    });

    {
        let mut w = World::new();
        w.spawn_batch((0..10_000).map(|_| bundle()));
        let mut q = w.query::<(&Velocity, &mut Position)>();
        suite.bench_throughput("simple_iter 10k: tessera", n, || {
            for (v, mut p) in q.iter_mut(&mut w) {
                p.0[0] += v.0[0];
            }
        });
        let mut h = hecs::World::new();
        h.spawn_batch((0..10_000).map(|_| bundle())).for_each(drop);
        suite.bench_throughput("simple_iter 10k: hecs", n, || {
            for (_, (v, p)) in h.query_mut::<(&Velocity, &mut Position)>() {
                p.0[0] += v.0[0];
            }
        });
    }

    {
        let mut w = World::new();
        frag!(w, spawn, Fa, Fb, Fc, Fd, Fe, Ff, Fg, Fh, Fi, Fj, Fk, Fl, Fm, Fn, Fo, Fp, Fq, Fr, Fs, Ft, Fu, Fv, Fw, Fx, Fy, Fz);
        let mut q = w.query::<&mut Data>();
        let n520 = Throughput::Elements(520);
        suite.bench_throughput("frag_iter 26x20: tessera", n520, || {
            for mut d in q.iter_mut(&mut w) {
                d.0 *= 2.0;
            }
        });
        let mut h = hecs::World::new();
        frag!(h, spawn, Ga, Gb, Gc, Gd, Ge, Gf, Gg, Gh, Gi, Gj, Gk, Gl, Gm, Gn, Go, Gp, Gq, Gr, Gs, Gt, Gu, Gv, Gw, Gx, Gy, Gz);
        suite.bench_throughput("frag_iter 26x20: hecs", n520, || {
            for (_, d) in h.query_mut::<&mut Data>() {
                d.0 *= 2.0;
            }
        });
    }

    {
        let mut w = World::new();
        let es = w.spawn_batch((0..10_000).map(|_| (A(0.0),)));
        suite.bench_throughput("add_remove 10k: tessera", n, || {
            for e in &es {
                w.insert(*e, (B(0.0),));
            }
            for e in &es {
                w.remove::<B>(*e);
            }
        });
        let mut h = hecs::World::new();
        let hs: Vec<hecs::Entity> = h.spawn_batch((0..10_000).map(|_| (A(0.0),))).collect();
        suite.bench_throughput("add_remove 10k: hecs", n, || {
            for e in &hs {
                h.insert_one(*e, B(0.0)).unwrap();
            }
            for e in &hs {
                h.remove_one::<B>(*e).unwrap();
            }
        });
    }

    {
        let n1k = Throughput::Elements(1000);
        let mut w = World::new();
        w.spawn_batch((0..1000).map(|_| bundle()));
        let mut seq = Schedule::new().single_threaded();
        seq.add_system(|mut q: Query<&mut Transform>| {
            for mut t in q.iter_mut() {
                heavy(&mut t.0);
            }
        });
        suite.bench_throughput("heavy_compute 1k: tessera sequential", n1k, || seq.run(&mut w));
        let mut par = Schedule::new();
        par.add_system(|mut q: Query<&mut Transform>| q.par_for_each_mut(64, |mut t| heavy(&mut t.0)));
        suite.bench_throughput("heavy_compute 1k: tessera par_for_each_mut", n1k, || par.run(&mut w));
        let mut h = hecs::World::new();
        h.spawn_batch((0..1000).map(|_| bundle())).for_each(drop);
        suite.bench_throughput("heavy_compute 1k: hecs sequential", n1k, || {
            for (_, t) in h.query_mut::<&mut Transform>() {
                heavy(&mut t.0);
            }
        });
    }

    {
        // Three systems over three separate component pairs: they can all run at once.
        let mut w = World::new();
        w.spawn_batch((0..10_000).map(|_| (A(0.0), B(0.0))));
        w.spawn_batch((0..10_000).map(|_| (A(0.0), B(0.0), C(0.0))));
        w.spawn_batch((0..10_000).map(|_| (A(0.0), B(0.0), C(0.0), D(0.0))));
        w.spawn_batch((0..10_000).map(|_| (A(0.0), B(0.0), C(0.0), E(0.0))));
        let build = |s: &mut Schedule| {
            s.add_system(|mut q: Query<(&mut A, &mut B)>| {
                for (mut a, mut b) in q.iter_mut() {
                    std::mem::swap(&mut a.0, &mut b.0);
                }
            });
            s.add_system(|mut q: Query<(&mut C, &mut D)>| {
                for (mut c, mut d) in q.iter_mut() {
                    std::mem::swap(&mut c.0, &mut d.0);
                }
            });
            s.add_system(|mut q: Query<(&mut C, &mut E)>| {
                for (mut c, mut e) in q.iter_mut() {
                    std::mem::swap(&mut c.0, &mut e.0);
                }
            });
        };
        let mut seq = Schedule::new().single_threaded();
        build(&mut seq);
        let mut par = Schedule::new();
        build(&mut par);
        suite.bench("schedule 3 systems 40k: tessera single-threaded", || seq.run(&mut w));
        suite.bench("schedule 3 systems 40k: tessera parallel", || par.run(&mut w));
    }

    let report = suite.into_report();
    println!("{}\n", fmt_env(&report.environment));
    print!("{}", render_table(&report.results));
    if let Some(path) = std::env::args().skip(1).find(|a| a.ends_with(".json")) {
        report.save(std::path::Path::new(&path)).expect("write report");
    }
}
