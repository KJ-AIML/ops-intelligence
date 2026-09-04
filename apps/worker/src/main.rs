//! Worker binary.
//!
//! `import` preserves CSV evidence; `process` drains received signals into Events
//! for the configured tenant and reports failures and remaining nonterminal work.
//!
//! ponytail: two subcommands need no argument-parsing dependency; add one when
//! options grow beyond these positional arguments.

use anyhow::{bail, Context, Result};
use ops_core::domains::raw_signals::RawSignal;
use ops_core::domains::sources::SourceType;
use ops_core::ids::SourceId;
use ops_core::ports::{
    Clock, InsertOutcome, OrganizationRepository, RawSignalRepository, SourceRepository,
    SystemClock,
};
use ops_persistence::PgStore;
use std::collections::BTreeMap;
use std::path::PathBuf;

const USAGE: &str = "\
usage: ops-worker import <path-to-csv>
       ops-worker process

  import   Ingest a CSV/spreadsheet export as RawSignals (idempotent; safe to re-run).
  process  Normalize received RawSignals for the configured tenant, then exit.
";

#[tokio::main]
async fn main() -> Result<()> {
    // A missing .env is fine — real deployments inject the environment directly.
    let _ = dotenvy::dotenv();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("import") => {
            let path: PathBuf = args.get(2).context("missing <path-to-csv>")?.into();
            import(path).await
        }
        Some("process") if args.len() == 2 => process().await,
        Some(other) => {
            eprint!("{USAGE}");
            bail!("unknown subcommand: {other}");
        }
        None => {
            eprint!("{USAGE}");
            bail!("no subcommand given");
        }
    }
}

async fn process() -> Result<()> {
    let database_url =
        std::env::var("DATABASE_URL").context("DATABASE_URL is not set; see README.md")?;
    let org_slug =
        std::env::var("DEFAULT_ORGANIZATION_SLUG").unwrap_or_else(|_| "pilot-org".into());
    let pool = ops_persistence::connect(&database_url).await?;
    ops_persistence::run_migrations(&pool).await?;
    let store = PgStore::new(pool);
    let organization = store
        .ensure_by_slug(&org_slug, "Pilot Organization")
        .await?;
    let report = ops_core::normalization::process_received(
        &store,
        organization.id,
        &ops_source_csv::CsvNormalizer,
        &SystemClock,
    )
    .await?;
    println!(
        "processing complete: {} ({})",
        organization.slug, organization.id
    );
    println!("  events new       {}", report.processed);
    println!("  signals failed   {}", report.failed);
    let mut pending = 0;
    let mut failed = 0;
    for (status, count) in store.count_by_status(organization.id).await? {
        println!("  {status:<12} {count:>4}");
        let parsed: ops_core::ProcessingStatus = status.parse()?;
        if !parsed.is_terminal() {
            pending += count;
        }
        if parsed == ops_core::ProcessingStatus::Failed {
            failed += count;
        }
    }
    if pending > 0 || failed > 0 {
        bail!("tenant needs attention: {pending} nonterminal signals, {failed} failed signals (see raw_signals.processing_error)");
    }
    Ok(())
}

async fn import(path: PathBuf) -> Result<()> {
    let database_url = std::env::var("DATABASE_URL")
        .context("DATABASE_URL is not set (copy the config block from README.md into .env)")?;
    let org_slug =
        std::env::var("DEFAULT_ORGANIZATION_SLUG").unwrap_or_else(|_| "pilot-org".to_string());

    let clock = SystemClock;

    // Read and validate the file before touching the database, so a malformed
    // file cannot leave a half-finished import behind.
    let signals = ops_source_csv::read_signals_from_path(&path)
        .with_context(|| format!("reading {}", path.display()))?;
    if signals.is_empty() {
        tracing::warn!(file = %path.display(), "no data rows found");
    }

    let pool = ops_persistence::connect(&database_url)
        .await
        .context("connecting to PostgreSQL")?;
    ops_persistence::run_migrations(&pool)
        .await
        .context("applying migrations")?;
    let store = PgStore::new(pool);

    let organization = store
        .ensure_by_slug(&org_slug, "Pilot Organization")
        .await
        .context("ensuring organization")?;

    tracing::info!(
        organization = %organization.slug,
        organization_id = %organization.id,
        file = %path.display(),
        rows = signals.len(),
        "starting import"
    );

    // One Source per originating system named in the CSV. They are all of type
    // csv_import: this is an offline import of a Grafana export, which is a
    // different input from a live Grafana integration and should not masquerade
    // as one (decision 0001).
    let mut sources: BTreeMap<String, SourceId> = BTreeMap::new();
    for signal in &signals {
        if !sources.contains_key(&signal.origin) {
            let name = format!("csv:{}", signal.origin);
            let source = store
                .ensure(organization.id, SourceType::CsvImport, &name)
                .await
                .with_context(|| format!("ensuring source {name}"))?;
            sources.insert(signal.origin.clone(), source.id);
        }
    }

    let received_at = clock.now();
    let mut inserted = 0usize;
    let mut duplicates = 0usize;
    let mut per_source: BTreeMap<&str, (usize, usize)> = BTreeMap::new();

    for signal in &signals {
        let source_id = sources[&signal.origin];

        // received_at is when WE received it, not when the alert fired. The alert's
        // own timestamp stays in the payload and becomes Event.occurred_at during
        // normalisation — correlation windows must never be computed from ingest
        // time for a historical import.
        let raw = RawSignal::received(
            organization.id,
            source_id,
            signal.external_id.clone(),
            ops_source_csv::CONTENT_TYPE,
            signal.payload.clone(),
            received_at,
        );

        let entry = per_source.entry(signal.origin.as_str()).or_insert((0, 0));
        match store.insert_if_new(&raw).await.with_context(|| {
            format!(
                "inserting CSV row {} (external_id {:?})",
                signal.row_number, signal.external_id
            )
        })? {
            InsertOutcome::Inserted(id) => {
                tracing::debug!(row = signal.row_number, raw_signal_id = %id, "inserted");
                inserted += 1;
                entry.0 += 1;
            }
            InsertOutcome::Duplicate => {
                tracing::debug!(
                    row = signal.row_number,
                    external_id = ?signal.external_id,
                    "duplicate, suppressed at ingestion"
                );
                duplicates += 1;
                entry.1 += 1;
            }
        }
    }

    for source_id in sources.values() {
        store
            .touch_last_seen(organization.id, *source_id, received_at)
            .await?;
    }

    let statuses = store.count_by_status(organization.id).await?;

    println!("import complete: {}", path.display());
    println!(
        "  organization      {} ({})",
        organization.slug, organization.id
    );
    println!("  csv rows read     {}", signals.len());
    println!("  raw signals new   {inserted}");
    println!("  duplicates        {duplicates}  (suppressed at ingestion)");
    println!("  sources");
    for (origin, (new, dup)) in &per_source {
        println!("    csv:{origin:<16} new {new:>3}  duplicate {dup:>3}");
    }
    println!("  raw_signals by processing_status (whole tenant)");
    for (status, count) in &statuses {
        println!("    {status:<12} {count:>4}");
    }

    tracing::info!(inserted, duplicates, "import complete");
    Ok(())
}
