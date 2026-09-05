// Wire types, mirroring apps/server/src/dto.rs.
// Kept hand-written and small: generating them would be more machinery than the
// pilot's surface justifies.

export type IncidentStatus = "open" | "acknowledged" | "recovered" | "resolved";
export type Severity = "debug" | "info" | "warning" | "critical";

export interface SourceNoise {
  source_id: string;
  name: string;
  source_type: string;
  signals: number;
  events: number;
  incidents: number;
  share_percent: number;
}

export interface RecurringPattern {
  fingerprint: string;
  environment: string | null;
  service: string | null;
  resource: string | null;
  event_family: string;
  occurrences: number;
  last_seen_at: string;
  currently_active: boolean;
}

export interface Summary {
  window: { from: string | null; to: string | null };
  raw_signals: number;
  events: number;
  events_without_incident: number;
  incidents: number;
  open: number;
  acknowledged: number;
  recovered: number;
  resolved: number;
  critical: number;
  needs_attention: number;
  noisiest_sources: SourceNoise[];
  recurring: RecurringPattern[];
}

export interface Incident {
  id: string;
  title: string;
  status: IncidentStatus;
  severity: Severity;
  environment: string | null;
  service: string | null;
  resource: string | null;
  event_family: string;
  event_count: number;
  source_count: number;
  started_at: string;
  last_event_at: string;
  recovered_at: string | null;
  reopened_count: number;
  occurrences: number;
  attention_score: number;
}

export interface Evidence {
  event_id: string;
  relation: "trigger" | "duplicate" | "recovery" | "update";
  occurred_at: string;
  severity: Severity;
  state: string;
  title: string;
  message: string | null;
  source_name: string;
  external_id: string | null;
  raw_signal_id: string;
  raw_payload: Record<string, unknown>;
}

export interface IncidentDetail extends Incident {
  timeline: Evidence[];
}

export interface EventRow {
  id: string;
  occurred_at: string;
  severity: Severity;
  state: string;
  event_family: string;
  title: string;
  message: string | null;
  environment: string | null;
  service: string | null;
  resource: string | null;
  external_id: string | null;
  source_id: string;
  raw_signal_id: string;
}

export interface Source {
  id: string;
  name: string;
  source_type: string;
  enabled: boolean;
  last_seen_at: string | null;
  has_ingest_token: boolean;
}

export interface CreatedSource extends Source {
  ingest_token: string;
  ingest_url: string;
}

const BASE = "/api/v1";

async function request<T>(path: string, init?: RequestInit): Promise<T> {
  const response = await fetch(`${BASE}${path}`, {
    headers: { "content-type": "application/json" },
    ...init,
  });
  if (!response.ok) {
    // The server returns { error } for anything it can explain. Surface that
    // rather than a bare status code.
    let message = `${response.status} ${response.statusText}`;
    try {
      const body = (await response.json()) as { error?: string };
      if (body.error) message = body.error;
    } catch {
      /* non-JSON error body; keep the status line */
    }
    throw new Error(message);
  }
  return (await response.json()) as T;
}

export const api = {
  summary: () => request<Summary>("/operations/summary"),

  incidents: (params: Record<string, string | number | undefined> = {}) => {
    const query = new URLSearchParams();
    for (const [key, value] of Object.entries(params)) {
      if (value !== undefined && value !== "") query.set(key, String(value));
    }
    const qs = query.toString();
    return request<Incident[]>(`/incidents${qs ? `?${qs}` : ""}`);
  },

  incident: (id: string) => request<IncidentDetail>(`/incidents/${id}`),

  acknowledge: (id: string) =>
    request<Incident>(`/incidents/${id}/acknowledge`, { method: "POST" }),

  resolve: (id: string) =>
    request<Incident>(`/incidents/${id}/resolve`, { method: "POST" }),

  events: (params: Record<string, string | number | undefined> = {}) => {
    const query = new URLSearchParams();
    for (const [key, value] of Object.entries(params)) {
      if (value !== undefined && value !== "") query.set(key, String(value));
    }
    const qs = query.toString();
    return request<EventRow[]>(`/events${qs ? `?${qs}` : ""}`);
  },

  sources: () => request<Source[]>("/sources"),

  createSource: (name: string) =>
    request<CreatedSource>("/sources", {
      method: "POST",
      body: JSON.stringify({ name }),
    }),

  setSourceEnabled: (id: string, enabled: boolean) =>
    request<Source>(`/sources/${id}`, {
      method: "PATCH",
      body: JSON.stringify({ enabled }),
    }),
};

// ---------------------------------------------------------------- formatting

export function formatTime(iso: string): string {
  return new Date(iso).toLocaleString(undefined, {
    month: "short",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
    hour12: false,
  });
}

export function formatDuration(fromIso: string, toIso: string | null): string {
  const end = toIso ? new Date(toIso).getTime() : Date.now();
  const seconds = Math.max(0, Math.round((end - new Date(fromIso).getTime()) / 1000));
  if (seconds < 60) return `${seconds}s`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes}m ${seconds % 60}s`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours}h ${minutes % 60}m`;
  return `${Math.floor(hours / 24)}d ${hours % 24}h`;
}
