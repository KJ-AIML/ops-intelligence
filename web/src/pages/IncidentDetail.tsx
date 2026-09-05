import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Link, useParams } from "react-router-dom";
import { api, formatDuration, formatTime } from "../api";
import {
  ErrorNotice,
  Loading,
  RelationTag,
  Scope,
  SeverityBadge,
  StatusBadge,
} from "../components";

export default function IncidentDetail() {
  const { id = "" } = useParams();
  const queryClient = useQueryClient();

  const incident = useQuery({
    queryKey: ["incident", id],
    queryFn: () => api.incident(id),
    enabled: id !== "",
  });

  const insights = useQuery({
    queryKey: ["insights", id],
    queryFn: () => api.incidentInsights(id),
    enabled: id !== "",
  });

  const invalidate = () => {
    void queryClient.invalidateQueries({ queryKey: ["incident", id] });
    void queryClient.invalidateQueries({ queryKey: ["incidents"] });
    void queryClient.invalidateQueries({ queryKey: ["summary"] });
  };

  const acknowledge = useMutation({ mutationFn: () => api.acknowledge(id), onSuccess: invalidate });
  const resolve = useMutation({ mutationFn: () => api.resolve(id), onSuccess: invalidate });

  if (incident.isPending) return <Loading what="incident" />;
  if (incident.isError) return <ErrorNotice error={incident.error} />;

  const i = incident.data;
  const failure = acknowledge.error ?? resolve.error;

  return (
    <>
      <p className="small">
        <Link to="/incidents" className="dim">
          ← Incidents
        </Link>
      </p>

      <h1>{i.title}</h1>
      <div className="toolbar">
        <SeverityBadge severity={i.severity} />
        <StatusBadge status={i.status} />
        <Scope environment={i.environment} service={i.service} resource={i.resource} />
        <span className="dim small">{i.event_family}</span>
        <span style={{ flex: 1 }} />
        <div className="actions">
          <button
            onClick={() => acknowledge.mutate()}
            disabled={i.status !== "open" || acknowledge.isPending}
            title={i.status !== "open" ? `Only an open incident can be acknowledged` : undefined}
          >
            Acknowledge
          </button>
          <button
            className="primary"
            onClick={() => resolve.mutate()}
            disabled={i.status === "resolved" || resolve.isPending}
          >
            Resolve
          </button>
        </div>
      </div>

      {failure && <ErrorNotice error={failure} />}

      <div className="grid-2 section">
        <div className="panel">
          <h2>Summary</h2>
          <dl className="kv">
            <dt>Started</dt>
            <dd>{formatTime(i.started_at)}</dd>
            <dt>Last activity</dt>
            <dd>{formatTime(i.last_event_at)}</dd>
            <dt>Duration</dt>
            <dd>
              {formatDuration(i.started_at, i.recovered_at)}
              {!i.recovered_at && i.status !== "resolved" && (
                <span className="dim"> and counting</span>
              )}
            </dd>
            <dt>Recovered</dt>
            <dd>{i.recovered_at ? formatTime(i.recovered_at) : <span className="dim">—</span>}</dd>
            <dt>Evidence</dt>
            <dd>
              {i.event_count} {i.event_count === 1 ? "event" : "events"} from {i.source_count}{" "}
              {i.source_count === 1 ? "source" : "sources"}
            </dd>
            <dt>Reopened</dt>
            <dd>{i.reopened_count > 0 ? `${i.reopened_count}×` : <span className="dim">never</span>}</dd>
          </dl>
        </div>

        <div className="panel">
          <h2>Pattern</h2>
          {i.occurrences > 1 ? (
            <p className="small" style={{ marginTop: 0 }}>
              This fingerprint has produced <strong>{i.occurrences} separate incidents</strong>. It
              keeps coming back rather than staying broken, which usually points at a recurring
              trigger rather than a permanent fault.
            </p>
          ) : (
            <p className="muted small" style={{ marginTop: 0 }}>
              First occurrence of this fingerprint.
            </p>
          )}
          {i.source_count > 1 && (
            <p className="small muted">
              Reported independently by {i.source_count} different sources, which is corroboration
              rather than duplication.
            </p>
          )}
          <p className="dim small" style={{ marginBottom: 0 }}>
            Triage rank {i.attention_score.toLocaleString()} — computed, not inferred.
          </p>
        </div>
      </div>

      {insights.data && insights.data.length > 0 && (
        <section className="section">
          {insights.data.map((insight) => (
            <div className="panel" key={insight.id} style={{ marginBottom: 12 }}>
              <h2>
                Interpretation
                {/* Provenance is never dropped: a model's reading of the facts
                    is a different class of claim from the facts themselves. */}
                <span className="dim" style={{ textTransform: "none", letterSpacing: 0 }}>
                  {" "}
                  · {insight.source === "ai" ? insight.model ?? "ai" : "computed"}
                </span>
              </h2>
              {insight.status === "failed" ? (
                <p className="muted small" style={{ margin: 0 }}>
                  No interpretation available: {insight.error}
                </p>
              ) : (
                <>
                  {insight.structured_payload?.actionability && (
                    <p style={{ marginTop: 0 }}>
                      <span className="badge st-acknowledged">
                        {insight.structured_payload.actionability.replace("_", " ")}
                      </span>
                    </p>
                  )}
                  <dl className="kv">
                    <dt>What happened</dt>
                    <dd>{insight.structured_payload?.explanation}</dd>
                    <dt>Likely impact</dt>
                    <dd>{insight.structured_payload?.likely_impact}</dd>
                    <dt>Check first</dt>
                    <dd>{insight.structured_payload?.suggested_check}</dd>
                  </dl>
                  <p className="dim small" style={{ marginBottom: 0 }}>
                    Interpretation, not a source fact. Generated by{" "}
                    {insight.provider}/{insight.model} ({insight.prompt_version}).
                  </p>
                </>
              )}
            </div>
          ))}
        </section>
      )}

      <section className="section">
        <div className="panel">
          <h2>Evidence timeline</h2>
          <ul className="timeline">
            {i.timeline.map((entry) => (
              <li key={entry.event_id}>
                <div>
                  <div className="mono small nowrap">{formatTime(entry.occurred_at)}</div>
                  <div className="dim small">{entry.source_name}</div>
                </div>
                <div>
                  <RelationTag relation={entry.relation} />
                  <div>
                    <SeverityBadge severity={entry.severity} />
                  </div>
                  <div className="dim small">{entry.state}</div>
                </div>
                <div>
                  <div>{entry.title}</div>
                  {entry.message && <div className="muted small">{entry.message}</div>}
                  <details>
                    <summary>
                      raw signal{entry.external_id ? ` · ${entry.external_id}` : ""}
                    </summary>
                    {/* The original payload, unmodified. Nothing downstream may
                        rewrite a source fact, so this is always checkable. */}
                    <pre>{JSON.stringify(entry.raw_payload, null, 2)}</pre>
                  </details>
                </div>
              </li>
            ))}
          </ul>
        </div>
      </section>
    </>
  );
}
