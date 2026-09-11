//! `decision-bench` — was eine Scheduling-Entscheidung auf dieser CPU kostet.
//!
//! Faehrt den echten `vig_core::Scheduler` durch eine lange, deterministische
//! Spur (Ankuenfte, Fertigstellungen, Takt, Verdraengung, Variantenwahl) und
//! misst jeden Aufruf von `on_event` mit der Wanduhr. Keine Inferenz, kein
//! Netz, kein Datenpfad: gemessen wird der Kern, nicht das Geraet.
//!
//! Gebaut wird es statisch fuer `aarch64` und auf Handys gefahren
//! (`tools/arm/build-and-run.sh`) — echte ARM-Kerne statt Emulation.
//!
//! ```text
//! decision-bench [--seconds N] [--repeats N] [--scenario gate|many16|many32|all] [--label TEXT]
//! ```
//!
//! `--seconds` ist simulierte Zeit. Jede Zeile beginnend mit `CSV,` ist
//! maschinenlesbar.

#![allow(
    clippy::print_stdout,
    clippy::arithmetic_side_effects,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::integer_division,
    clippy::too_many_lines
)]

use std::time::Instant as Wall;
use vig_core::Duration;
use vig_core::profile::SafetyMargin;
use vig_sim::decision::{EventKind, Summary, gate_m3, many_models, median, summarize};
use vig_sim::harness::{run_observed, vig};
use vig_sim::scenario::Scenario;

const SEED: u64 = 0x5EED_A4C4;

struct Args {
    seconds: u64,
    repeats: usize,
    scenario: String,
    label: String,
}

fn parse_args() -> Args {
    let mut args = Args {
        seconds: 300,
        repeats: 3,
        scenario: "all".to_owned(),
        label: "unbenannt".to_owned(),
    };
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let value = it.next().unwrap_or_default();
        match flag.as_str() {
            "--seconds" => args.seconds = value.parse().unwrap_or(args.seconds),
            "--repeats" => args.repeats = value.parse().unwrap_or(args.repeats).max(1),
            "--scenario" => args.scenario = value,
            "--label" => args.label = value,
            other => println!("unbekannte Option {other} ignoriert"),
        }
    }
    args
}

/// Eine Zeile aus `/proc/self/status`, etwa `VmHWM` oder `Cpus_allowed_list`.
fn proc_status(key: &str) -> String {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with(key))
                .map(|l| l.split_once(':').map_or("", |(_, v)| v).trim().to_owned())
        })
        .unwrap_or_else(|| "?".to_owned())
}

/// Der Kern, auf dem dieser Prozess zuletzt lief (Feld 39 von `/proc/self/stat`).
fn last_cpu() -> String {
    std::fs::read_to_string("/proc/self/stat")
        .ok()
        .and_then(|s| {
            // Der Befehlsname steht in Klammern und kann Leerzeichen
            // enthalten; gezaehlt wird ab der schliessenden Klammer.
            let rest = s.rsplit_once(')')?.1;
            rest.split_whitespace().nth(36).map(ToOwned::to_owned)
        })
        .unwrap_or_else(|| "?".to_owned())
}

/// Die CPU-Kennungen aus `/proc/cpuinfo`, gezaehlt.
fn cpu_parts() -> String {
    let Ok(info) = std::fs::read_to_string("/proc/cpuinfo") else {
        return "?".to_owned();
    };
    let mut parts: Vec<(String, u32)> = Vec::new();
    for line in info.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let key = key.trim();
        if key == "CPU part" || key == "model name" {
            let value = value.trim().to_owned();
            match parts.iter_mut().find(|(v, _)| *v == value) {
                Some((_, n)) => *n += 1,
                None => parts.push((value, 1)),
            }
        }
    }
    parts
        .iter()
        .map(|(v, n)| format!("{n}x {v}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Was zwei aufeinanderfolgende Uhrablesungen kosten, Median in ns.
///
/// Jede Einzelmessung enthaelt diesen Betrag. Er wird berichtet und nicht
/// abgezogen: abziehen hiesse, eine zweite Schaetzung in die erste zu rechnen.
fn timer_overhead() -> u64 {
    let mut samples: Vec<u64> = (0..200_000)
        .map(|_| {
            let t = Wall::now();
            t.elapsed().as_nanos() as u64
        })
        .collect();
    summarize(&mut samples).p50
}

struct Repeat {
    per_kind: [Summary; 5],
    all: Summary,
    wall_ns: u64,
    received: u64,
    forwarded: u64,
    superseded: u64,
    stale: u64,
}

impl Repeat {
    /// Die Zusammenfassung einer Ereignisart.
    fn kind(&self, kind: EventKind) -> Summary {
        self.per_kind.get(kind.index()).copied().unwrap_or_default()
    }
}

fn run_once(scenario: &Scenario, seed: u64) -> Repeat {
    let mut samples: [Vec<u64>; 5] = Default::default();
    let wall = Wall::now();
    let result = run_observed(
        scenario,
        vig(scenario, SafetyMargin::DEFAULT),
        String::new(),
        seed,
        |g, now, event, actions| {
            let kind = EventKind::of(&event);
            let t = Wall::now();
            g.on_event(now, event, actions);
            let dt = t.elapsed().as_nanos() as u64;
            if let Some(bucket) = samples.get_mut(kind.index()) {
                bucket.push(dt);
            }
        },
    );
    let wall_ns = wall.elapsed().as_nanos() as u64;
    let mut all: Vec<u64> = samples.iter().flatten().copied().collect();
    let per_kind = samples.map(|mut s| summarize(&mut s));
    Repeat {
        per_kind,
        all: summarize(&mut all),
        wall_ns,
        received: result.metrics.received,
        forwarded: result.metrics.forwarded,
        superseded: result.metrics.superseded,
        stale: result.metrics.stale,
    }
}

fn scenarios(args: &Args) -> Vec<(&'static str, Scenario)> {
    let d = Duration::from_nanos_unbounded(args.seconds.saturating_mul(1_000_000_000));
    let mut out = Vec::new();
    let all = args.scenario == "all";
    if all || args.scenario == "gate" {
        out.push(("gate-m3 (4 Modelle)", gate_m3(d)));
    }
    if all || args.scenario == "many16" {
        out.push(("16 Modelle", many_models(16, d)));
    }
    if all || args.scenario == "many32" {
        out.push(("32 Modelle", many_models(32, d)));
    }
    out
}

fn main() {
    let args = parse_args();
    println!("decision-bench: was eine Scheduling-Entscheidung auf dieser CPU kostet");
    println!(
        "Ziel {}-{}{} · Label {} · erlaubte Kerne {} · CPU {}",
        std::env::consts::ARCH,
        std::env::consts::OS,
        if cfg!(target_env = "musl") {
            "-musl"
        } else {
            ""
        },
        args.label,
        proc_status("Cpus_allowed_list"),
        cpu_parts()
    );
    let overhead = timer_overhead();
    println!("Uhr: {overhead} ns je Ablesepaar (steckt in jeder Einzelmessung, nicht abgezogen)");
    println!(
        "{} s simulierte Zeit je Lauf, {} Wiederholungen nach einem verworfenen Aufwaermlauf\n",
        args.seconds, args.repeats
    );

    for (name, scenario) in scenarios(&args) {
        let _warmup = run_once(&scenario, SEED);
        let mut repeats = Vec::new();
        for r in 0..args.repeats {
            let rep = run_once(&scenario, SEED.wrapping_add(r as u64));
            for kind in EventKind::ALL {
                let s = rep.kind(kind);
                if s.count == 0 {
                    continue;
                }
                println!(
                    "CSV,{},{},{},{},{},{},{},{},{},{}",
                    args.label,
                    scenario.name,
                    scenario.streams.len(),
                    r,
                    kind.label(),
                    s.count,
                    s.p50,
                    s.p99,
                    s.p999,
                    s.max
                );
            }
            repeats.push(rep);
        }

        println!("  {name}");
        println!(
            "    Ereignis   |  Anzahl | p50 ns | p99 ns | p99,9 ns |  max ns | Spannweite p99 ueber Wdh."
        );
        println!(
            "    -----------|---------|--------|--------|----------|---------|--------------------------"
        );
        for kind in EventKind::ALL {
            let col = |f: fn(&Summary) -> u64| -> Vec<u64> {
                repeats.iter().map(|r| f(&r.kind(kind))).collect()
            };
            let counts = col(|s| s.count);
            if counts.iter().all(|c| *c == 0) {
                continue;
            }
            let p99 = col(|s| s.p99);
            println!(
                "    {:<10} | {:>7} | {:>6} | {:>6} | {:>8} | {:>7} | {}–{}",
                kind.label(),
                median(&counts),
                median(&col(|s| s.p50)),
                median(&p99),
                median(&col(|s| s.p999)),
                median(&col(|s| s.max)),
                p99.iter().min().copied().unwrap_or(0),
                p99.iter().max().copied().unwrap_or(0),
            );
        }
        let all_p99: Vec<u64> = repeats.iter().map(|r| r.all.p99).collect();
        let decisions_per_s: Vec<u64> = repeats
            .iter()
            .map(|r| {
                r.all
                    .count
                    .saturating_mul(1_000_000_000)
                    .checked_div(r.all.total.max(1))
                    .unwrap_or(0)
            })
            .collect();
        let wall_ms: Vec<u64> = repeats.iter().map(|r| r.wall_ns / 1_000_000).collect();
        println!(
            "    alle       | {:>7} | {:>6} | {:>6} | {:>8} | {:>7} | {}–{}",
            median(&repeats.iter().map(|r| r.all.count).collect::<Vec<_>>()),
            median(&repeats.iter().map(|r| r.all.p50).collect::<Vec<_>>()),
            median(&all_p99),
            median(&repeats.iter().map(|r| r.all.p999).collect::<Vec<_>>()),
            median(&repeats.iter().map(|r| r.all.max).collect::<Vec<_>>()),
            all_p99.iter().min().copied().unwrap_or(0),
            all_p99.iter().max().copied().unwrap_or(0),
        );
        if let Some(first) = repeats.first() {
            println!(
                "    Entscheidungen/s im Kern (Median): {} · Lauf {} ms Wanduhr · Last: angenommen {} weitergereicht {} verdraengt {} verworfen {}",
                median(&decisions_per_s),
                median(&wall_ms),
                first.received,
                first.forwarded,
                first.superseded,
                first.stale,
            );
            println!(
                "CSV-RUN,{},{},{},{},{},{}",
                args.label,
                scenario.name,
                scenario.streams.len(),
                median(&decisions_per_s),
                median(&all_p99),
                median(&wall_ms)
            );
        }
        println!();
    }

    println!(
        "Spitzen-RSS {} · zuletzt auf Kern {}",
        proc_status("VmHWM"),
        last_cpu()
    );
}
