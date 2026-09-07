//! Pilot Lab commands.
//!
//! A replay is a full pass of the real pipeline — normalization, correlation,
//! optionally reasoning — over previously captured signals, inside a throwaway
//! tenant. Nothing is re-implemented for the lab: if the lab and production
//! disagreed, the lab would be measuring the wrong engine.

use crate::{normalizers, reasoning};
use anyhow::{Context, Result};
use ops_core::domains::pilot::{compare, PilotRun, RunStats};
use ops_core::ports::{Clock, PilotRepository, SystemClock};
use ops_core::OrganizationId;
use ops_persistence::PgStore;

/// Recorded on every run so two results are comparable on purpose rather than
/// by accident. Bump it when correlation or normalization behaviour changes.
pub const ENGINE_VERSION: &str = env!("CARGO_PKG_VERSION");

pub async fn create_dataset(
    store: &PgStore,
    organization_id: OrganizationId,
    name: &str,
    source: Option<&str>,
) -> Result<()> {
    let dataset = store
        .create_dataset(organization_id, name, None, source, SystemClock.now())
        .await
        .context("creating dataset")?;

    println!("dataset created: {}", dataset.name);
    println!("  id           {}", dataset.id);
    println!("  signals      {}", dataset.signal_count);
    println!("  frozen       {}", dataset.is_frozen());
    if let Some(source) = source {
        println!("  source       {source}");
    }
    Ok(())
}

pub async fn list_datasets(store: &PgStore, organization_id: OrganizationId) -> Result<()> {
    let datasets = store.list_datasets(organization_id).await?;
    if datasets.is_empty() {
        println!("no datasets yet — capture some signals, then: pilot dataset create <name>");
        return Ok(());
    }
    println!("{:<28}{:>9}  {:<12} runs", "DATASET", "SIGNALS", "FROZEN");
    for dataset in &datasets {
        let runs = store.list_runs(dataset.id).await?;
        println!(
            "{:<28}{:>9}  {:<12} {}",
            dataset.name,
            dataset.signal_count,
            if dataset.is_frozen() { "yes" } else { "no" },
            runs.len()
        );
    }
    Ok(())
}

pub async fn replay(
    store: &PgStore,
    organization_id: OrganizationId,
    dataset_name: &str,
    label: &str,
    with_ai: bool,
) -> Result<()> {
    let dataset = store
        .find_dataset(organization_id, dataset_name)
        .await?
        .with_context(|| format!("no dataset named {dataset_name:?}"))?;

    let provider = with_ai.then(reasoning::provider_from_env);
    let descriptor = provider.as_ref().map(|p| p.descriptor());
    let ai = descriptor
        .as_ref()
        .filter(|_| provider.as_ref().is_some_and(|p| p.is_enabled()))
        .map(|d| (d.provider.as_str(), d.model.as_str()));

    let clock = SystemClock;
    let run = store
        .start_run(dataset.id, label, ENGINE_VERSION, ai, clock.now())
        .await
        .context("starting run")?;

    println!("replaying {} as {}", dataset.name, run.label);
    println!("  tenant       {} (isolated)", run.target_organization_id);
    println!("  engine       {ENGINE_VERSION}");

    match execute(store, &run, provider, with_ai).await {
        Ok(stats) => {
            store.finish_run(run.id, &stats, None, clock.now()).await?;
            print_stats(&stats);
        }
        Err(e) => {
            // The run is recorded as failed rather than vanishing, so a broken
            // replay is visible next to the ones that worked.
            store
                .finish_run(
                    run.id,
                    &RunStats::default(),
                    Some(&e.to_string()),
                    clock.now(),
                )
                .await?;
            return Err(e);
        }
    }
    Ok(())
}

async fn execute(
    store: &PgStore,
    run: &PilotRun,
    provider: Option<Box<dyn ops_core::ReasoningProvider>>,
    with_ai: bool,
) -> Result<RunStats> {
    let clock = SystemClock;
    let org = run.target_organization_id;

    let (replayed, suppressed) = store.replay_signals_into_run(run, clock.now()).await?;

    // The real pipeline, not a lab copy of it.
    let processing = ops_core::normalization::process_received(
        store,
        org,
        &normalizers::DispatchingNormalizer,
        &clock,
    )
    .await?;

    while ops_core::ports::IncidentRepository::correlate_next(store, org).await? {}

    if with_ai {
        if let Some(provider) = provider {
            // Bounded: a replay must not turn into an unbounded bill.
            reasoning::run(store, org, provider.as_ref(), &clock, 25).await?;
        }
    }

    let mut stats = store.collect_stats(org).await?;
    stats.signals_replayed = replayed;
    stats.signals_suppressed = suppressed;
    stats.events_failed = processing.failed as i64;
    Ok(stats)
}

fn print_stats(s: &RunStats) {
    println!(
        "  signals      {} replayed, {} suppressed",
        s.signals_replayed, s.signals_suppressed
    );
    println!("  events       {} ({} failed)", s.events, s.events_failed);
    println!(
        "  incidents    {} ({} open, {} recovered, {} critical)",
        s.incidents, s.open, s.recovered, s.critical
    );
    println!(
        "  relations    {} duplicate, {} recovery",
        s.duplicate_relations, s.recovery_relations
    );
    println!("  recurring    {} patterns", s.recurrence_patterns);
    println!("  no incident  {} events", s.events_without_incident);
    if let Some(ratio) = s.compression() {
        // Reported next to correctness, never instead of it: a correlator that
        // merges everything scores best here and is useless.
        println!("  compression  {ratio} events per incident");
    }
    if s.insights_ok + s.insights_failed > 0 {
        println!(
            "  insights     {} ok, {} failed",
            s.insights_ok, s.insights_failed
        );
    }
}

pub async fn compare_runs(store: &PgStore, a: &str, b: &str) -> Result<()> {
    let before = store
        .find_run(a)
        .await?
        .with_context(|| format!("no run labelled {a:?}"))?;
    let after = store
        .find_run(b)
        .await?
        .with_context(|| format!("no run labelled {b:?}"))?;

    println!("comparing {} -> {}", before.label, after.label);
    println!(
        "  engine       {} -> {}",
        before.engine_version, after.engine_version
    );
    println!(
        "  ai           {} -> {}",
        describe_ai(&before),
        describe_ai(&after)
    );

    if before.dataset_id != after.dataset_id {
        // Comparing runs over different inputs produces numbers that look like
        // a result and mean nothing.
        println!();
        println!("  WARNING: these runs replayed DIFFERENT datasets; the deltas below");
        println!("           are not a like-for-like comparison.");
    }

    let deltas = compare(&before.stats, &after.stats);
    println!();
    if deltas.is_empty() {
        println!("  no change — identical results");
        return Ok(());
    }
    println!(
        "  {:<26}{:>9}{:>9}{:>9}",
        "FIELD", "BEFORE", "AFTER", "CHANGE"
    );
    for d in &deltas {
        println!(
            "  {:<26}{:>9}{:>9}{:>+9}",
            d.field,
            d.before,
            d.after,
            d.change()
        );
    }
    println!();
    println!("  Fewer incidents is not automatically better. Check the groupings.");
    Ok(())
}

fn describe_ai(run: &PilotRun) -> String {
    if !run.ai_enabled {
        return "off".into();
    }
    format!(
        "{}/{}",
        run.ai_provider.as_deref().unwrap_or("?"),
        run.ai_model.as_deref().unwrap_or("?")
    )
}
