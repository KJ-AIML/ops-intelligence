//! Canonical `event_family` classifier (decision 0002, layer 2).
//!
//! Vendor-agnostic and pure: no Grafana, no Azure, no database, no clock. This
//! is the whole point of the layer — the taxonomy stays testable without any
//! vendor code.

use super::FamilyHints;
use crate::domains::events::{EventFamily, EventState};
use std::str::FromStr;

/// Label an operator can set on an alert to bypass classification entirely.
pub const FAMILY_LABEL: &str = "event_family";

/// Condition families — things that describe something being *wrong*.
///
/// Order matters: first match wins, so the more specific patterns come first.
/// `Availability` is last because its vocabulary ("down", "timeout") is the
/// broadest and would otherwise swallow more precise matches.
const CONDITION_TABLE: &[(EventFamily, &[&str])] = &[
    (
        EventFamily::Certificate,
        &["certificate", "tls", "ssl", "expiring"],
    ),
    (EventFamily::Backup, &["backup", "restore job"]),
    (
        EventFamily::Latency,
        &["latency", "p95", "p99", "response time", "slow"],
    ),
    (
        EventFamily::ErrorRate,
        &[
            "5xx",
            "error rate",
            "errorrate",
            "exception rate",
            "error log",
        ],
    ),
    (EventFamily::SaturationCpu, &["cpu"]),
    (
        EventFamily::SaturationMemory,
        // "oomkilled" is the Kubernetes termination reason verbatim; "oom killed"
        // covers the hyphenated and spaced spellings after normalization. Bare
        // "oom" is deliberately absent — it is a substring of ordinary words.
        &["memory", "rss", "heap", "oomkilled", "oom killed"],
    ),
    (
        EventFamily::SaturationDisk,
        // "pvc" catches PersistentVolumeClaim alerts, which name neither a disk
        // nor a volume in their title.
        &["disk", "inode", "free space", "volume", "pvc"],
    ),
    (
        EventFamily::Connectivity,
        &[
            "dns",
            "connection pool",
            "connection error",
            "replication",
            "network",
            "servfail",
        ],
    ),
    (
        EventFamily::Availability,
        // Process-death vocabulary lives here, and last: a crash, a panic or a
        // fatal is the service failing to stay up. It sits below the saturation
        // families on purpose, so an OOM kill is classified by its cause
        // (memory) rather than by its symptom (the container died).
        &[
            "down",
            "unreachable",
            "offline",
            "healthcheck",
            "health check",
            "connection refused",
            "timeout",
            "service restored",
            "crash",
            "panic",
            "fatal",
        ],
    ),
];

/// Lifecycle families — things that merely *happened*.
///
/// Informational signals are classified against this table ONLY. They never fall
/// through to the condition table, because an operational notice that happens to
/// mention DNS is not a DNS problem: "Alert rule updated: DNSResolutionFailures"
/// is `unclassified`, not `connectivity`, and "Datasource health check ok" is not
/// an availability event.
const LIFECYCLE_TABLE: &[(EventFamily, &[&str])] = &[
    (EventFamily::Backup, &["backup", "restore"]),
    (
        EventFamily::Job,
        &[
            "deployment",
            "deployed",
            "scheduled",
            "job",
            "autoscale",
            "scaled out",
            "maintenance window",
            "report generated",
            "monitor added",
        ],
    ),
];

/// Resolution order (decision 0002):
///   1. explicit `event_family` label — always wins
///   2. per-source mapping rules from configuration
///   3. canonical keyword table for the signal's state
///   4. `unclassified` — never a guess
pub fn classify(
    hints: &FamilyHints,
    state: EventState,
    family_rules: &std::collections::BTreeMap<String, EventFamily>,
) -> EventFamily {
    // 1. explicit operator label
    if let Some(labelled) = hints.labels.get(FAMILY_LABEL) {
        return EventFamily::from_str(labelled.trim()).unwrap_or(EventFamily::Unclassified);
    }

    // 2. per-source rules, keyed on the vendor's own rule/metric/category name
    for key in [
        hints.metric_name.as_deref(),
        hints.vendor_category.as_deref(),
        Some(hints.title.as_str()),
    ]
    .into_iter()
    .flatten()
    {
        if let Some(family) = family_rules.get(key) {
            return *family;
        }
    }

    // 3. keyword table for this state
    let table = match state {
        EventState::Informational => LIFECYCLE_TABLE,
        EventState::Firing | EventState::Resolved => CONDITION_TABLE,
    };

    // Hyphens become spaces so one keyword covers both spellings a vendor might
    // use: "crash-looping" and "crash looping", "error-log" and "error log",
    // "health-check" and "health check". Keywords below are therefore always
    // written with spaces.
    let haystack = {
        let mut s = hints.title.to_lowercase();
        if let Some(message) = &hints.message {
            s.push(' ');
            s.push_str(&message.to_lowercase());
        }
        s.replace('-', " ")
    };

    for (family, keywords) in table {
        if keywords.iter().any(|k| haystack.contains(k)) {
            return *family;
        }
    }

    // 4. explicit fallback
    EventFamily::Unclassified
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn hints(title: &str) -> FamilyHints {
        FamilyHints {
            title: title.to_string(),
            message: None,
            labels: BTreeMap::new(),
            metric_name: None,
            monitor_type: None,
            vendor_category: None,
        }
    }

    fn classify_firing(title: &str) -> EventFamily {
        classify(&hints(title), EventState::Firing, &BTreeMap::new())
    }

    fn classify_info(title: &str) -> EventFamily {
        classify(&hints(title), EventState::Informational, &BTreeMap::new())
    }

    #[test]
    fn service_restored_is_availability_and_invalid_explicit_label_does_not_guess() {
        assert_eq!(
            classify_firing("[Up] Service restored"),
            EventFamily::Availability
        );
        assert_eq!(classify_firing("Restore job failed"), EventFamily::Backup);
        let mut h = hints("CPU high");
        h.labels
            .insert(FAMILY_LABEL.into(), "unknown-family".into());
        assert_eq!(
            classify(&h, EventState::Firing, &BTreeMap::new()),
            EventFamily::Unclassified
        );
    }

    #[test]
    fn explicit_label_beats_everything() {
        let mut h = hints("CPU percentage greater than 90");
        h.labels
            .insert(FAMILY_LABEL.to_string(), "latency".to_string());
        assert_eq!(
            classify(&h, EventState::Firing, &BTreeMap::new()),
            EventFamily::Latency
        );
    }

    #[test]
    fn per_source_rule_beats_the_keyword_table() {
        let mut rules = BTreeMap::new();
        rules.insert(
            "CPU percentage greater than 90".to_string(),
            EventFamily::Connectivity,
        );
        assert_eq!(
            classify(
                &hints("CPU percentage greater than 90"),
                EventState::Firing,
                &rules
            ),
            EventFamily::Connectivity
        );
    }

    #[test]
    fn specific_patterns_win_over_broad_ones() {
        // "[Down] Connection refused" would match availability either way, but
        // a latency alert must not be swallowed by the availability vocabulary.
        assert_eq!(
            classify_firing("[RESOLVED] HighAPILatency p95 above 800ms"),
            EventFamily::Latency
        );
        assert_eq!(
            classify_firing("Elevated5xxRate above 2 percent"),
            EventFamily::ErrorRate
        );
        assert_eq!(
            classify_firing("SQL connection pool utilization above 90"),
            EventFamily::Connectivity
        );
        assert_eq!(
            classify_firing("[mail-relay] [Down] Connection timeout"),
            EventFamily::Availability
        );
    }

    #[test]
    fn saturation_families_stay_separate() {
        assert_eq!(
            classify_firing("CPU percentage over 90"),
            EventFamily::SaturationCpu
        );
        assert_eq!(
            classify_firing("Memory percentage over 88"),
            EventFamily::SaturationMemory
        );
        assert_eq!(
            classify_firing("Disk free space less than 10 percent"),
            EventFamily::SaturationDisk
        );
    }

    #[test]
    fn informational_signals_never_fall_through_to_condition_families() {
        // The two cases that make the state gate necessary.
        assert_eq!(
            classify_info("Alert rule updated: DNSResolutionFailures"),
            EventFamily::Unclassified
        );
        assert_eq!(
            classify_info("Datasource health check ok (prometheus)"),
            EventFamily::Unclassified
        );
        // ...while the same words in a firing signal DO classify.
        assert_eq!(
            classify_firing("DNSResolutionFailures above threshold"),
            EventFamily::Connectivity
        );
    }

    #[test]
    fn informational_lifecycle_signals_still_classify() {
        assert_eq!(
            classify_info("Deployment completed: payment-api"),
            EventFamily::Job
        );
        assert_eq!(
            classify_info("Nightly maintenance window started"),
            EventFamily::Job
        );
        assert_eq!(
            classify_info("Nightly backup completed on db-prod-02"),
            EventFamily::Backup
        );
    }

    #[test]
    fn unknown_input_is_unclassified_not_guessed() {
        assert_eq!(
            classify_firing("something entirely novel"),
            EventFamily::Unclassified
        );
        assert_eq!(
            classify_info("Daily infrastructure digest"),
            EventFamily::Unclassified
        );
    }
}
