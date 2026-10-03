use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tessera::system::Query;
use tessera::{Added, Changed, Commands, Entity, Local, Res, ResMut, Schedule, With, Without, World};

#[derive(Debug, Clone, Copy, PartialEq)]
struct Pos(f32);
#[derive(Debug, Clone, Copy, PartialEq)]
struct Vel(f32);
#[derive(Debug, Clone, Copy, PartialEq)]
struct Health(i32);
struct Player;
#[derive(Default)]
struct Frame(u32);

fn movement(mut q: Query<(&mut Pos, &Vel)>) {
    for (mut p, v) in q.iter_mut() {
        p.0 += v.0;
    }
}

#[test]
fn function_systems_with_every_kind_of_parameter() {
    let mut w = World::new();
    w.insert_resource(Frame(0));
    let a = w.spawn((Pos(0.0), Vel(1.0)));
    let b = w.spawn((Pos(10.0), Vel(-1.0), Health(3)));
    let mut s = Schedule::new();
    s.add_system(movement);
    s.add_system(|mut f: ResMut<Frame>| f.0 += 1);
    s.add_system(|q: Query<(Entity, &Health)>, mut c: Commands| {
        for (e, h) in q.iter() {
            if h.0 <= 0 {
                c.despawn(e);
            }
        }
    });
    s.add_system(|mut q: Query<&mut Health>| {
        for mut h in q.iter_mut() {
            h.0 -= 1;
        }
    });
    s.add_system(|mut runs: Local<u32>, f: Res<Frame>| {
        *runs += 1;
        assert_eq!(*runs, f.0, "a local keeps its value between runs");
    });
    for _ in 0..3 {
        s.run(&mut w);
    }
    assert_eq!(w.get::<Pos>(a), Some(&Pos(3.0)));
    assert_eq!(w.resource::<Frame>().unwrap().0, 3);
    // Health 3 -> 0 after three runs; the despawn system sees 0 on the fourth.
    assert!(w.contains(b));
    s.run(&mut w);
    assert!(!w.contains(b), "despawned by a command");
}

#[test]
fn change_detection_is_per_system() {
    let mut w = World::new();
    let e = w.spawn((Pos(0.0),));
    let seen = Arc::new(AtomicUsize::new(0));
    let added = Arc::new(AtomicUsize::new(0));
    let (s1, a1) = (seen.clone(), added.clone());
    let mut s = Schedule::new();
    s.add_system(move |q: Query<&Pos, Changed<Pos>>, n: Query<Entity, Added<Pos>>| {
        s1.fetch_add(q.iter().count(), Ordering::SeqCst);
        a1.fetch_add(n.iter().count(), Ordering::SeqCst);
    });
    s.run(&mut w);
    assert_eq!((seen.load(Ordering::SeqCst), added.load(Ordering::SeqCst)), (1, 1), "first run: added and changed");
    s.run(&mut w);
    assert_eq!(seen.load(Ordering::SeqCst), 1, "nothing changed in between");
    w.get_mut::<Pos>(e).unwrap().0 = 5.0;
    s.run(&mut w);
    assert_eq!((seen.load(Ordering::SeqCst), added.load(Ordering::SeqCst)), (2, 1));
    // Reading through get_mut without writing is not a change.
    let _ = w.get_mut::<Pos>(e).unwrap().0;
    s.run(&mut w);
    assert_eq!(seen.load(Ordering::SeqCst), 2);
}

#[test]
fn resource_change_detection() {
    let mut w = World::new();
    w.insert_resource(Frame(0));
    let flags = Arc::new(std::sync::Mutex::new(Vec::new()));
    let f2 = flags.clone();
    let mut s = Schedule::new();
    s.add_system(move |f: Res<Frame>| f2.lock().unwrap().push((f.is_added(), f.is_changed())));
    s.run(&mut w);
    s.run(&mut w);
    w.resource_mut::<Frame>().unwrap().0 = 9;
    s.run(&mut w);
    assert_eq!(*flags.lock().unwrap(), vec![(true, true), (false, false), (false, true)]);
}

#[test]
fn exclusive_systems_see_the_whole_world() {
    let mut w = World::new();
    let mut s = Schedule::new();
    s.add_system(|w: &mut World| {
        w.spawn((Pos(1.0),));
    });
    s.add_system(|q: Query<&Pos>| assert_eq!(q.iter().count(), 1, "the spawn happened before"));
    s.run(&mut w);
    assert_eq!(w.len(), 1);
}

#[test]
#[should_panic(expected = "conflicts with another of its parameters")]
fn conflicting_parameters_in_one_system_are_refused() {
    let mut w = World::new();
    let mut s = Schedule::new();
    s.add_system(|_a: Query<&mut Pos>, _b: Query<&Pos>| {});
    s.run(&mut w);
}

#[test]
fn disjoint_queries_may_share_a_system() {
    let mut w = World::new();
    w.spawn((Pos(0.0), Player));
    w.spawn((Pos(5.0),));
    let mut s = Schedule::new();
    // The player follows the first other entity.
    s.add_system(|mut player: Query<&mut Pos, With<Player>>, others: Query<&Pos, Without<Player>>| {
        let target = others.single().0;
        for mut p in player.iter_mut() {
            p.0 = target;
        }
    });
    s.run(&mut w);
    let mut q = w.query_filtered::<&Pos, With<Player>>();
    assert_eq!(q.iter(&w).next(), Some(&Pos(5.0)));
}

#[test]
fn stages_group_independent_systems() {
    let mut w = World::new();
    let mut s = Schedule::new();
    s.add_system(|_: Query<&mut Pos>| {}); // 0
    s.add_system(|_: Query<&mut Vel>| {}); // 1: independent of 0
    s.add_system(|_: Query<(&Pos, &Vel)>| {}); // 2: after 0 and 1
    s.add_system(|_: Query<&mut Health>| {}); // 3: independent of all
    s.add_system(|_: ResMut<Frame>| {}); // 4: independent
    s.add_system(|_: Res<Frame>| {}); // 5: after 4
    let st = s.stages(&mut w);
    let sizes: Vec<usize> = st.iter().map(|x| x.len()).collect();
    assert_eq!(sizes, vec![4, 2]);
}

/// Independent systems overlap in time; conflicting ones never do.
#[test]
fn the_parallel_executor_overlaps_only_what_does_not_conflict() {
    if tessera::system::pool().threads() < 2 {
        eprintln!("SKIPPED: one thread");
        return;
    }
    let mut w = World::new();
    w.spawn((Pos(0.0), Vel(0.0), Health(0)));
    w.insert_resource(Frame(0));
    let active_pos = Arc::new(AtomicUsize::new(0));
    let max_pos = Arc::new(AtomicUsize::new(0));
    let running = Arc::new(AtomicUsize::new(0));
    let max_running = Arc::new(AtomicUsize::new(0));
    let mut s = Schedule::new();
    for i in 0..6 {
        let (ap, mp, r, mr) = (active_pos.clone(), max_pos.clone(), running.clone(), max_running.clone());
        if i % 2 == 0 {
            // Writers of Pos: must run one at a time.
            s.add_system(move |mut q: Query<&mut Pos>| {
                let now = ap.fetch_add(1, Ordering::SeqCst) + 1;
                mp.fetch_max(now, Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(20));
                for mut p in q.iter_mut() {
                    p.0 += 1.0;
                }
                ap.fetch_sub(1, Ordering::SeqCst);
            });
        } else {
            // Readers of Health: may all run together and alongside the writers.
            s.add_system(move |q: Query<&Health>| {
                let now = r.fetch_add(1, Ordering::SeqCst) + 1;
                mr.fetch_max(now, Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(20));
                assert_eq!(q.iter().count(), 1);
                r.fetch_sub(1, Ordering::SeqCst);
            });
        }
    }
    let t = std::time::Instant::now();
    s.run(&mut w);
    let took = t.elapsed();
    assert_eq!(max_pos.load(Ordering::SeqCst), 1, "two Pos writers ran at once");
    assert!(max_running.load(Ordering::SeqCst) >= 2, "readers never overlapped");
    assert_eq!(w.query::<&Pos>().iter(&w).next(), Some(&Pos(3.0)));
    // Three writers in a row (60 ms) with the readers alongside, not six in a row (120 ms).
    assert!(took < Duration::from_millis(110), "{took:?}");
}

#[test]
fn a_panicking_system_panics_the_run_after_the_others_finish() {
    let mut w = World::new();
    let done = Arc::new(AtomicUsize::new(0));
    let d2 = done.clone();
    let mut s = Schedule::new();
    s.add_system(|_: Query<&Pos>| panic!("boom"));
    s.add_system(move |_: Query<&Vel>| {
        std::thread::sleep(Duration::from_millis(10));
        d2.fetch_add(1, Ordering::SeqCst);
    });
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| s.run(&mut w)));
    assert!(r.is_err());
    assert_eq!(done.load(Ordering::SeqCst), 1);
}

#[test]
fn parallel_iteration_visits_every_entity_once() {
    let mut w = World::new();
    let n = if cfg!(miri) { 600 } else { 10_000 };
    for i in 0..n {
        if i % 3 == 0 {
            w.spawn((Pos(i as f32), Vel(1.0), Health(0)));
        } else {
            w.spawn((Pos(i as f32), Vel(1.0)));
        }
    }
    let mut s = Schedule::new();
    let batch = if cfg!(miri) { 64 } else { 256 };
    s.add_system(move |mut q: Query<(&mut Pos, &Vel)>| q.par_for_each_mut(batch, |(mut p, v)| p.0 += v.0));
    s.run(&mut w);
    let mut q = w.query::<&Pos>();
    let sum: f64 = q.iter(&w).map(|p| p.0 as f64).sum();
    assert_eq!(sum, (0..n).map(|i| i as f64 + 1.0).sum::<f64>());
}

#[test]
fn single_threaded_schedules_give_the_same_result() {
    let run = |s: &mut Schedule| {
        let mut w = World::new();
        for i in 0..100 {
            w.spawn((Pos(i as f32), Vel(2.0)));
        }
        s.add_system(movement);
        s.add_system(movement);
        s.run(&mut w);
        let mut q = w.query::<&Pos>();
        q.iter(&w).map(|p| p.0).sum::<f32>()
    };
    assert_eq!(run(&mut Schedule::new()), run(&mut Schedule::new().single_threaded()));
}
