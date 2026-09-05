import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { api, formatTime } from "../api";
import type { Severity } from "../api";
import { Empty, ErrorNotice, Loading, Scope, SeverityBadge } from "../components";

const SEVERITIES: (Severity | "")[] = ["", "critical", "warning", "info", "debug"];

/**
 * Deliberately secondary (tech sheet 18). This exists for debugging and for
 * trust — being able to see the normalized events behind an incident — not as
 * the primary way to work.
 */
export default function Events() {
  const [severity, setSeverity] = useState("");
  const [service, setService] = useState("");

  const events = useQuery({
    queryKey: ["events", { severity, service }],
    queryFn: () =>
      api.events({
        severity: severity || undefined,
        service: service || undefined,
        limit: 200,
      }),
  });

  return (
    <>
      <h1>Events</h1>
      <p className="subtitle">
        Normalized facts, one per accepted signal. Vendor severity and state dialects have already
        been mapped to canonical values here.
      </p>

      <div className="toolbar">
        <select value={severity} onChange={(e) => setSeverity(e.target.value)}>
          {SEVERITIES.map((s) => (
            <option key={s} value={s}>
              {s === "" ? "any severity" : s}
            </option>
          ))}
        </select>
        <input
          placeholder="filter by service"
          value={service}
          onChange={(e) => setService(e.target.value)}
        />
        {events.data && <span className="dim small">{events.data.length} shown</span>}
      </div>

      {events.isPending ? (
        <Loading what="events" />
      ) : events.isError ? (
        <ErrorNotice error={events.error} />
      ) : events.data.length === 0 ? (
        <Empty>No events match this filter.</Empty>
      ) : (
        <div className="panel" style={{ padding: 0 }}>
          <table>
            <thead>
              <tr>
                <th>Occurred</th>
                <th>Severity</th>
                <th>State</th>
                <th>Family</th>
                <th>Title</th>
                <th>Scope</th>
                <th>External id</th>
              </tr>
            </thead>
            <tbody>
              {events.data.map((event) => (
                <tr key={event.id}>
                  <td className="mono small nowrap">{formatTime(event.occurred_at)}</td>
                  <td>
                    <SeverityBadge severity={event.severity} />
                  </td>
                  <td className="small muted">{event.state}</td>
                  <td className="mono small muted">{event.event_family}</td>
                  <td>{event.title}</td>
                  <td>
                    <Scope
                      environment={event.environment}
                      service={event.service}
                      resource={event.resource}
                    />
                  </td>
                  <td className="mono small dim">{event.external_id ?? "—"}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </>
  );
}
