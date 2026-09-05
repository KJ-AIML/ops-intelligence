import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { Link, useNavigate } from "react-router-dom";
import { api, formatDuration, formatTime } from "../api";
import type { IncidentStatus, Severity } from "../api";
import { Empty, ErrorNotice, Loading, Scope, SeverityBadge, StatusBadge } from "../components";

const STATUSES: (IncidentStatus | "")[] = ["", "open", "acknowledged", "recovered", "resolved"];
const SEVERITIES: (Severity | "")[] = ["", "critical", "warning", "info", "debug"];

export default function Incidents() {
  const [status, setStatus] = useState<string>("");
  const [severity, setSeverity] = useState<string>("");
  const [sort, setSort] = useState<"recent" | "attention">("attention");
  const navigate = useNavigate();

  const incidents = useQuery({
    queryKey: ["incidents", { status, severity, sort }],
    queryFn: () =>
      api.incidents({
        status: status || undefined,
        severity: severity || undefined,
        sort: sort === "attention" ? "attention" : undefined,
        limit: 200,
      }),
  });

  return (
    <>
      <h1>Incidents</h1>
      <p className="subtitle">
        Correlated from events. One incident per environment, service, resource and event family —
        different families are never merged.
      </p>

      <div className="toolbar">
        <select value={status} onChange={(e) => setStatus(e.target.value)}>
          {STATUSES.map((s) => (
            <option key={s} value={s}>
              {s === "" ? "any status" : s}
            </option>
          ))}
        </select>
        <select value={severity} onChange={(e) => setSeverity(e.target.value)}>
          {SEVERITIES.map((s) => (
            <option key={s} value={s}>
              {s === "" ? "any severity" : s}
            </option>
          ))}
        </select>
        <select value={sort} onChange={(e) => setSort(e.target.value as "recent" | "attention")}>
          <option value="attention">sort: needs attention</option>
          <option value="recent">sort: most recent</option>
        </select>
        {incidents.data && <span className="dim small">{incidents.data.length} shown</span>}
      </div>

      {incidents.isPending ? (
        <Loading what="incidents" />
      ) : incidents.isError ? (
        <ErrorNotice error={incidents.error} />
      ) : incidents.data.length === 0 ? (
        <Empty>No incidents match this filter.</Empty>
      ) : (
        <div className="panel" style={{ padding: 0 }}>
          <table>
            <thead>
              <tr>
                <th>Severity</th>
                <th>Status</th>
                <th>Incident</th>
                <th>Scope</th>
                <th>Family</th>
                <th className="num">Events</th>
                <th className="num">Seen</th>
                <th>Duration</th>
                <th>Last activity</th>
              </tr>
            </thead>
            <tbody>
              {incidents.data.map((incident) => (
                <tr
                  key={incident.id}
                  className="clickable"
                  onClick={() => navigate(`/incidents/${incident.id}`)}
                >
                  <td>
                    <SeverityBadge severity={incident.severity} />
                  </td>
                  <td>
                    <StatusBadge status={incident.status} />
                  </td>
                  <td>
                    <Link to={`/incidents/${incident.id}`}>{incident.title}</Link>
                    {incident.reopened_count > 0 && (
                      <span className="dim small"> · reopened {incident.reopened_count}×</span>
                    )}
                  </td>
                  <td>
                    <Scope
                      environment={incident.environment}
                      service={incident.service}
                      resource={incident.resource}
                    />
                  </td>
                  <td className="mono small muted">{incident.event_family}</td>
                  <td className="num">{incident.event_count}</td>
                  <td className="num">{incident.occurrences > 1 ? `${incident.occurrences}×` : "—"}</td>
                  <td className="nowrap muted small">
                    {formatDuration(incident.started_at, incident.recovered_at)}
                  </td>
                  <td className="nowrap muted small">{formatTime(incident.last_event_at)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </>
  );
}
