import { useQuery } from "@tanstack/react-query";
import { Link } from "react-router-dom";
import { api, formatDuration, formatTime } from "../api";
import {
  Empty,
  ErrorNotice,
  Loading,
  Panel,
  Scope,
  SeverityBadge,
  StatusBadge,
} from "../components";

/**
 * The Operations View. It exists to answer five questions without the engineer
 * having to construct a query:
 *   What happened?  What is still unresolved?  What keeps happening?
 *   Which source is creating noise?  What should I investigate first?
 */
export default function Overview() {
  const summary = useQuery({ queryKey: ["summary"], queryFn: api.summary });

  // Ranked server-side by the same deterministic function the API exposes, so
  // this list and the incidents page cannot disagree about what matters.
  const attention = useQuery({
    queryKey: ["incidents", "attention"],
    queryFn: () => api.incidents({ sort: "attention", limit: 8 }),
  });

  if (summary.isPending) return <Loading what="operations summary" />;
  if (summary.isError) return <ErrorNotice error={summary.error} />;

  const s = summary.data;

  return (
    <>
      <h1>Today</h1>
      <p className="subtitle">
        {s.raw_signals.toLocaleString()} signals became {s.events.toLocaleString()} events and{" "}
        {s.incidents.toLocaleString()} incidents.
      </p>

      <section className="section stats">
        <div className="stat">
          <div className="value">{s.events.toLocaleString()}</div>
          <div className="label">events</div>
        </div>
        <div className="stat">
          <div className="value">{s.incidents.toLocaleString()}</div>
          <div className="label">incidents</div>
        </div>
        <div className="stat attention">
          <div className="value">{s.needs_attention}</div>
          <div className="label">need attention</div>
        </div>
        <div className="stat critical">
          <div className="value">{s.critical}</div>
          <div className="label">critical</div>
        </div>
        <div className="stat ok">
          <div className="value">{s.recovered}</div>
          <div className="label">recovered</div>
        </div>
        <div className="stat">
          <div className="value">{s.acknowledged + s.resolved}</div>
          <div className="label">ack / resolved</div>
        </div>
      </section>

      <section className="section">
        <h2>What to investigate first</h2>
        {attention.isPending ? (
          <Loading what="incidents" />
        ) : attention.isError ? (
          <ErrorNotice error={attention.error} />
        ) : attention.data.length === 0 ? (
          <Empty>Nothing is open. Every incident has recovered or been resolved.</Empty>
        ) : (
          <div className="panel" style={{ padding: 0 }}>
            <table>
              <thead>
                <tr>
                  <th>Severity</th>
                  <th>Status</th>
                  <th>Incident</th>
                  <th>Scope</th>
                  <th className="num">Events</th>
                  <th className="num">Seen</th>
                  <th>Last activity</th>
                </tr>
              </thead>
              <tbody>
                {attention.data.map((incident) => (
                  <tr key={incident.id}>
                    <td>
                      <SeverityBadge severity={incident.severity} />
                    </td>
                    <td>
                      <StatusBadge status={incident.status} />
                    </td>
                    <td>
                      <Link to={`/incidents/${incident.id}`}>{incident.title}</Link>
                      {incident.source_count > 1 && (
                        <span className="dim small"> · {incident.source_count} sources</span>
                      )}
                    </td>
                    <td>
                      <Scope
                        environment={incident.environment}
                        service={incident.service}
                        resource={incident.resource}
                      />
                    </td>
                    <td className="num">{incident.event_count}</td>
                    <td className="num">
                      {incident.occurrences > 1 ? `${incident.occurrences}×` : "—"}
                    </td>
                    <td className="nowrap muted small">{formatTime(incident.last_event_at)}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </section>

      <div className="grid-2">
        <Panel title="What keeps happening">
          {s.recurring.length === 0 ? (
            <p className="muted small">No fingerprint has recurred yet.</p>
          ) : (
            <table>
              <tbody>
                {s.recurring.map((r) => (
                  <tr key={r.fingerprint}>
                    <td>
                      <div className="mono small">
                        {[r.service, r.resource].filter(Boolean).join(" / ") || "—"}
                      </div>
                      <div className="dim small">
                        {r.event_family}
                        {r.environment ? ` · ${r.environment}` : ""}
                        {r.currently_active ? " · active now" : ""}
                      </div>
                    </td>
                    <td className="num nowrap">
                      <strong>{r.occurrences}×</strong>
                      <div className="dim small">{formatTime(r.last_seen_at)}</div>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
        </Panel>

        <Panel title="Where the noise comes from">
          {s.noisiest_sources.length === 0 ? (
            <p className="muted small">No sources have reported yet.</p>
          ) : (
            <table>
              <tbody>
                {s.noisiest_sources.map((source) => (
                  <tr key={source.source_id}>
                    <td>
                      <div>{source.name}</div>
                      {/* Volume share against incident share: this is what makes
                          a chatty source visible as chatty rather than busy. */}
                      <div className="dim small">
                        {source.signals} signals · {source.incidents} incidents
                      </div>
                      <div className="bar">
                        <div style={{ width: `${Math.min(100, source.share_percent)}%` }} />
                      </div>
                    </td>
                    <td className="num nowrap">{source.share_percent.toFixed(1)}%</td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
        </Panel>
      </div>

      <section className="section" style={{ marginTop: 20 }}>
        <div className="panel">
          <h2>Signal accounting</h2>
          <p className="muted small" style={{ margin: 0 }}>
            {s.events_without_incident.toLocaleString()} of {s.events.toLocaleString()} events
            produced no incident — informational notices and recoveries with no matching open
            incident. Every signal is accounted for; none were dropped.
            {s.incidents > 0 && (
              <>
                {" "}
                Correlation compressed {s.events.toLocaleString()} events into{" "}
                {s.incidents.toLocaleString()} incidents
                {s.recovered > 0 && (
                  <>
                    , {s.recovered} of which recovered on their own
                    {s.open > 0 && ` and ${s.open} of which are still open`}
                  </>
                )}
                .
              </>
            )}
          </p>
        </div>
      </section>

      {s.window.from && (
        <p className="dim small">
          Window {formatTime(s.window.from)} →{" "}
          {s.window.to ? formatTime(s.window.to) : "now"} (
          {formatDuration(s.window.from, s.window.to)})
        </p>
      )}
    </>
  );
}
