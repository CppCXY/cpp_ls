//! **Where a start's time goes, and what the first request costs** — the probe behind the design note's numbers.
//!
//! ```text
//! $env:CPPLS_REAL_PROJECT = "E:\EmmyLuaCodeStyle"
//! cargo test --release -p cpp_code_analysis --test start_shape -- --ignored --nocapture
//! ```
//!
//! **The loop is the pump's own shape**, not an approximation of it: a wave prepared under a read lock and committed
//! under the writer, then the drain rendered and committed, then the cooking slice claimed, gathered, rendered,
//! **refused** and committed. The refusal matters: a file that can never be cooked leaves the cooking backlog only
//! because the pump says so, and a probe that leaves it out spins for ever — which it did, before this was faithful.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use cpp_code_analysis::{DiskFiles, OpenDocuments, Session, SessionFiles, WatchFilter};

fn project() -> Option<PathBuf> {
    let named = std::env::var_os("CPPLS_REAL_PROJECT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"E:\EmmyLuaCodeStyle"));
    named.is_dir().then_some(named)
}

fn ms(elapsed: Duration) -> f64 {
    elapsed.as_secs_f64() * 1000.0
}

#[test]
#[ignore = "opens a real project; run with --ignored"]
fn where_a_start_spends_its_time() {
    let Some(root) = project() else {
        eprintln!("skipped: no project (set CPPLS_REAL_PROJECT)");
        return;
    };

    // **1. The handshake** — the floor under the first request.
    let opened = Instant::now();
    let mut session = Session::open(
        root.clone(),
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&root),
    );
    let open = opened.elapsed();
    let all = session.project_files().to_vec();
    println!("\n=== {} project file(s)", all.len());
    println!("Session::open                        {:>8.1} ms", ms(open));

    // **2. The first request** — the hover path: open the buffer, catch up, cook, view, diagnose.
    let open_file = all
        .iter()
        .find(|path| path.to_string_lossy().ends_with(".cpp"))
        .cloned()
        .expect("the project has a source file");
    println!(
        "the file a reader has open            {}",
        open_file.display()
    );

    let text = std::fs::read_to_string(&open_file).unwrap_or_default();
    let opened_at = Instant::now();
    session.did_open(open_file.clone(), &text);
    println!(
        "did_open                             {:>8.1} ms",
        ms(opened_at.elapsed())
    );

    let catching = Instant::now();
    session.catch_up(&open_file);
    println!(
        "catch_up                             {:>8.1} ms",
        ms(catching.elapsed())
    );

    let cooking = Instant::now();
    let cooked = session.cook(&open_file);
    println!(
        "cook (a unit walk + a parse)         {:>8.1} ms   {}",
        ms(cooking.elapsed()),
        if cooked.is_some() {
            "cooked"
        } else {
            "nothing"
        }
    );

    let viewing = Instant::now();
    let view = session.view(&open_file);
    println!(
        "view                                 {:>8.2} ms   {}",
        ms(viewing.elapsed()),
        if view.is_some() {
            "a reading"
        } else {
            "nothing"
        }
    );

    let diagnosing = Instant::now();
    let answer = session.diagnostics(&open_file);
    println!(
        "diagnostics (every check)            {:>8.1} ms   {} error(s), {} finding(s)",
        ms(diagnosing.elapsed()),
        answer.as_ref().map(|found| found.errors.len()).unwrap_or(0),
        answer.as_ref().map(|found| found.checks.len()).unwrap_or(0),
    );

    // **3. The pass**, in the pump's shape.
    session.add_project_files(all.clone());
    let started = Instant::now();
    let mut slices = 0usize;
    let mut whole_prepare = Duration::ZERO;
    let mut whole_commit = Duration::ZERO;
    let mut whole_drain = Duration::ZERO;
    let mut worst_commit = Duration::ZERO;
    let mut wave = 8usize;
    let mut stuck_at: Option<(usize, usize)> = None;

    while slices < 4000 {
        let work = session.pending_work();
        if work == 0 {
            break;
        }
        if stuck_at.is_none() && slices > 200 {
            stuck_at = Some((work, session.pending_cooking()));
        }

        let preparing = Instant::now();
        let prepared = session.prepare_a_wave(wave);
        whole_prepare += preparing.elapsed();
        let prepared_count = prepared.prepared.len();

        let committing = Instant::now();
        let (done, drain) = session.commit_a_wave(prepared, wave);
        let commit = committing.elapsed();
        whole_commit += commit;
        worst_commit = worst_commit.max(commit);
        let committed = done.len();

        // The drain and the cooking slice — both `&self` gathers and renders, then one write lock for the inserts.
        let draining = Instant::now();
        let drain = session.prepare_the_drain(drain);
        let claimed = session.claim_cooking(4);
        let asked = claimed.clone();
        let materials = session.materials_for(claimed);
        let rendered = session.render_them(materials);
        let built: Vec<PathBuf> = rendered
            .iter()
            .map(|rendered| rendered.indexed.summary.path.clone())
            .collect();
        let gathered = draining.elapsed();
        session.commit_the_drain(drain);
        session.refuse_the_cooks_that_built_nothing(&asked, &built);
        let _ = session.commit_them(rendered);
        whole_drain += draining.elapsed();

        if slices < 8 || slices % 500 == 0 {
            println!(
                "  slice {slices:>4}: prepared {prepared_count:>3}, committed {committed:>3}, asked {asked_count:>2}, built {built_count:>2}, queue {:>4}, cooking {:>3}, work {:>4}, gather {:>7.1} ms, commit {:>6.1} ms",
                session.pending(),
                session.pending_cooking(),
                work,
                ms(gathered),
                ms(commit),
                asked_count = asked.len(),
                built_count = built.len(),
            );
        }
        slices += 1;
        wave = (wave * 2).min(64);
    }
    let pass = started.elapsed();
    println!("--- the pass");
    println!("slices                               {:>8}", slices);
    println!("whole pass                           {:>8.1} ms", ms(pass));
    println!(
        "  prepare (read lock)                {:>8.1} ms",
        ms(whole_prepare)
    );
    println!(
        "  commit (write lock)                {:>8.1} ms",
        ms(whole_commit)
    );
    println!(
        "  drain + cooking (read lock)        {:>8.1} ms",
        ms(whole_drain)
    );
    println!(
        "  worst single commit                {:>8.1} ms",
        ms(worst_commit)
    );
    println!(
        "work left                            {:>8}",
        session.pending_work()
    );
    if let Some((work, cooking)) = stuck_at {
        println!(
            "work after 200 slices                {:>8} ({cooking} to cook)",
            work
        );
    }

    // **4. Steady state**, which is the floor a request pays forever after.
    let mut settled: Vec<Duration> = Vec::new();
    for _ in 0..20 {
        let asked = Instant::now();
        let _ = session.view(&open_file);
        settled.push(asked.elapsed());
    }
    settled.sort();
    println!("--- steady state (pump idle)");
    println!(
        "view, median                         {:>8.2} ms",
        ms(settled[10])
    );
    println!(
        "view, max                            {:>8.2} ms",
        ms(settled[19])
    );

    let mut diagnoses: Vec<Duration> = Vec::new();
    for _ in 0..10 {
        let asked = Instant::now();
        let _ = session.diagnostics(&open_file);
        diagnoses.push(asked.elapsed());
    }
    diagnoses.sort();
    println!(
        "diagnostics, median                  {:>8.1} ms",
        ms(diagnoses[5])
    );
    println!(
        "diagnostics, max                     {:>8.1} ms",
        ms(diagnoses[9])
    );

    // **5. A second start**, which is what a warm cache buys.
    drop(session);
    let reopened = Instant::now();
    let session = Session::open(
        root.clone(),
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&root),
    );
    println!("--- a second start");
    println!(
        "Session::open (warm)                 {:>8.1} ms",
        ms(reopened.elapsed())
    );
    // **6. The same pass again, on a warm cache** — the number that says whether the disk cache is doing its job.
    let mut session = session;
    session.add_project_files(all.clone());
    let warm_started = Instant::now();
    let mut warm_slices = 0usize;
    let mut warm_wave = 8usize;
    while session.pending_work() > 0 && warm_slices < 4000 {
        let prepared = session.prepare_a_wave(warm_wave);
        let (_, drain) = session.commit_a_wave(prepared, warm_wave);
        let drain = session.prepare_the_drain(drain);
        let claimed = session.claim_cooking(4);
        let asked = claimed.clone();
        let materials = session.materials_for(claimed);
        let rendered = session.render_them(materials);
        let built: Vec<PathBuf> = rendered
            .iter()
            .map(|rendered| rendered.indexed.summary.path.clone())
            .collect();
        session.commit_the_drain(drain);
        session.refuse_the_cooks_that_built_nothing(&asked, &built);
        let _ = session.commit_them(rendered);
        warm_slices += 1;
        warm_wave = (warm_wave * 2).min(64);
    }
    println!("--- the same pass on a warm cache");
    println!(
        "warm pass                            {:>8.1} ms  ({warm_slices} slices)",
        ms(warm_started.elapsed())
    );
    let _ = view;
}
