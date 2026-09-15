//! Label dialects a real Grafana estate uses.
//!
//! The canonical webhook contract (tech sheet 14) names a `severity` label and an
//! `environment` label. Real estates do not all use those names:
//!
//! - Grafana rules are frequently labelled `tier` rather than `severity`, because
//!   the label is also the notification-routing key and reads better as a tier.
//! - Alertmanager stamps the cluster identity as an `externalLabel` called
//!   `cluster`, so alerts forwarded from it carry no `environment` at all.
//!
//! Both dialects use the same *vocabulary* the engine already maps (`critical`,
//! `warning`, `info`); only the label name differs. Missing them is not a
//! mis-mapping that fails loudly — it silently downgrades every critical alert to
//! the "severity absent while firing" default, which is exactly the kind of quiet
//! wrongness this engine exists to avoid.
//!
//! The alert titles below are the standard Kubernetes operational vocabulary that
//! kube-prometheus-stack and hand-written Loki rules produce. They are here as a
//! classification fixture, not as anyone's topology.

use chrono::{DateTime, Utc};
use ops_core::domains::events::{EventFamily, Severity};
use ops_core::ports::SignalNormalizer;
use ops_core::{OrganizationId, RawSignal, SourceId, SourceType};
use ops_source_grafana::{split_batch, GrafanaNormalizer, CONTENT_TYPE};
use serde_json::{json, Value};

/// One alert, built the way Grafana's webhook contact point sends it.
fn alert(title: &str, labels: Value) -> Value {
    json!({
        "receiver": "ops-intelligence",
        "status": "firing",
        "alerts": [{
            "status": "firing",
            "labels": labels,
            "annotations": { "summary": format!("{title} in PRD") },
            "startsAt": "2026-09-15T09:42:10Z",
            "endsAt": "0001-01-01T00:00:00Z",
            "fingerprint": "3f2a9c1d8e7b6a50"
        }],
        "version": "1",
        "truncatedAlerts": 0
    })
}

/// Normalize inside one explicit tenant. The fingerprint test needs every event
/// to share an organization, or it would be measuring tenant isolation rather
/// than classification.
fn normalize_in(organization_id: OrganizationId, body: &Value) -> ops_core::Event {
    let now: DateTime<Utc> = "2030-01-01T00:00:00Z".parse().unwrap();
    let signal = split_batch(body).unwrap().remove(0);
    let raw = RawSignal::received(
        organization_id,
        SourceId::new(),
        Some(signal.external_id),
        CONTENT_TYPE,
        signal.payload,
        now,
    );
    GrafanaNormalizer
        .normalize(&raw, SourceType::Grafana, now)
        .unwrap()
}

fn normalize(body: &Value) -> ops_core::Event {
    normalize_in(OrganizationId::new(), body)
}

#[test]
fn a_tier_label_carries_severity_when_no_severity_label_exists() {
    for (tier, expected) in [
        ("critical", Severity::Critical),
        ("warning", Severity::Warning),
        ("info", Severity::Info),
    ] {
        let event = normalize(&alert(
            "Node disk > 85%",
            json!({ "alertname": "NodeDisk", "tier": tier }),
        ));
        assert_eq!(event.severity, expected, "tier={tier}");
    }
}

#[test]
fn an_explicit_severity_label_still_wins_over_tier() {
    // `severity` is the contract; `tier` is the fallback. A rule that sets both
    // must not have its declared severity overridden by a routing label.
    let event = normalize(&alert(
        "Node disk > 85%",
        json!({ "alertname": "NodeDisk", "severity": "critical", "tier": "warning" }),
    ));
    assert_eq!(event.severity, Severity::Critical);
}

#[test]
fn a_tier_value_that_is_not_a_severity_fails_loudly_rather_than_guessing() {
    // Routing tiers are not always severities. A tier of `security` says which
    // escalation path an alert takes, not how bad it is, and the engine has no
    // honest mapping for it.
    //
    // Reading `tier` at all changes what happens here: before, the label was
    // ignored and the alert silently took the absent-severity default. Now it is
    // a validation error, so the signal is stored with its reason, counted on the
    // Overview's failed-signals tile, and named in the day-one smoke replay the
    // runbook asks for. Losing one alert type visibly beats recording every one
    // of them at a severity nobody chose.
    //
    // The remedy is configuration, not code: map the token for that source.
    let now: DateTime<Utc> = "2030-01-01T00:00:00Z".parse().unwrap();
    let body = alert(
        "Auth-failure spike",
        json!({ "alertname": "AuthFailure", "tier": "security" }),
    );
    let signal = split_batch(&body).unwrap().remove(0);
    let raw = RawSignal::received(
        OrganizationId::new(),
        SourceId::new(),
        Some(signal.external_id),
        CONTENT_TYPE,
        signal.payload,
        now,
    );

    let error = GrafanaNormalizer
        .normalize(&raw, SourceType::Grafana, now)
        .expect_err("an unmappable tier must not be guessed at")
        .to_string();
    assert!(
        error.contains("security"),
        "the error must name the token an operator has to map: {error}"
    );
}

#[test]
fn a_cluster_label_carries_the_environment_when_no_environment_label_exists() {
    let event = normalize(&alert(
        "Node disk > 85%",
        json!({ "alertname": "NodeDisk", "severity": "critical", "cluster": "prd" }),
    ));
    assert_eq!(event.environment.as_deref(), Some("prd"));
}

#[test]
fn environment_and_env_still_win_over_cluster() {
    let event = normalize(&alert(
        "Node disk > 85%",
        json!({ "alertname": "NodeDisk", "severity": "critical",
                "environment": "production", "cluster": "prd" }),
    ));
    assert_eq!(event.environment.as_deref(), Some("production"));

    let event = normalize(&alert(
        "Node disk > 85%",
        json!({ "alertname": "NodeDisk", "severity": "critical",
                "env": "uat", "cluster": "prd" }),
    ));
    assert_eq!(event.environment.as_deref(), Some("uat"));
}

/// The standard Kubernetes alert vocabulary, and the family each belongs to.
///
/// `Auth-failure spike` is deliberately `Unclassified`: the canonical taxonomy has
/// no security family, and inventing one for a single rule would be a guess
/// (decision 0002). An operator who wants it grouped can set an explicit
/// `event_family` label, which always wins.
const VOCABULARY: &[(&str, EventFamily)] = &[
    ("App crash / panic / fatal", EventFamily::Availability),
    ("Pod crash-looping", EventFamily::Availability),
    ("Error-log spike", EventFamily::ErrorRate),
    ("HTTP 5xx spike", EventFamily::ErrorRate),
    ("DB / connection errors", EventFamily::Connectivity),
    (
        "Container OOMKilled (recent)",
        EventFamily::SaturationMemory,
    ),
    ("Node memory > 90%", EventFamily::SaturationMemory),
    ("Node CPU > 90%", EventFamily::SaturationCpu),
    ("Node disk > 85%", EventFamily::SaturationDisk),
    ("PVC > 85% full", EventFamily::SaturationDisk),
    ("Auth-failure spike", EventFamily::Unclassified),
];

#[test]
fn the_standard_kubernetes_alert_vocabulary_classifies() {
    for (title, expected) in VOCABULARY {
        let event = normalize(&alert(
            title,
            json!({ "alertname": "Probe", "tier": "critical" }),
        ));
        assert_eq!(event.event_family, *expected, "{title}");
    }
}

#[test]
fn distinct_problems_do_not_share_one_incident_fingerprint() {
    // The failure this guards: before the vocabulary above was classified, seven
    // of these eleven fell through to `Unclassified`. With no service or resource
    // on an aggregate alert, the fingerprint is environment + family alone, so all
    // seven merged into a single incident that could be declared recovered as a
    // whole — the over-merge the product exists to prevent (decision 0003).
    use ops_core::domains::incidents::IncidentFingerprint;
    use std::collections::BTreeSet;

    let org = OrganizationId::new();
    let mut unclassified = 0usize;
    let mut keys = BTreeSet::new();
    for (title, _) in VOCABULARY {
        let event = normalize_in(
            org,
            &alert(
                title,
                json!({ "alertname": "Probe", "tier": "critical", "cluster": "prd" }),
            ),
        );
        if event.event_family == EventFamily::Unclassified {
            unclassified += 1;
        }
        keys.insert(IncidentFingerprint::from_event(&event).key());
    }

    assert!(
        unclassified <= 1,
        "{unclassified} rules fell through to Unclassified; they would share one \
         fingerprint and merge into a single incident"
    );
    assert_eq!(
        keys.len(),
        7,
        "expected seven distinct fingerprints across the vocabulary"
    );
}
