import type { ReactNode } from "react";
import type { Incident, Severity } from "./api";

export function SeverityBadge({ severity }: { severity: Severity }) {
  return <span className={`badge sev-${severity}`}>{severity}</span>;
}

export function StatusBadge({ status }: { status: Incident["status"] }) {
  return <span className={`badge st-${status}`}>{status}</span>;
}

export function RelationTag({ relation }: { relation: string }) {
  return <span className={`rel rel-${relation}`}>{relation}</span>;
}

/** Where an incident is, in one line: production / payment-api / api-prod-01. */
export function Scope({
  environment,
  service,
  resource,
}: {
  environment: string | null;
  service: string | null;
  resource: string | null;
}) {
  const parts = [environment, service, resource].filter(Boolean);
  if (parts.length === 0) return <span className="dim">—</span>;
  return <span className="mono muted">{parts.join(" / ")}</span>;
}

export function Loading({ what }: { what: string }) {
  return <div className="notice">Loading {what}…</div>;
}

export function ErrorNotice({ error }: { error: unknown }) {
  const message = error instanceof Error ? error.message : String(error);
  return <div className="notice error">Could not load: {message}</div>;
}

export function Empty({ children }: { children: ReactNode }) {
  return <div className="notice">{children}</div>;
}

export function Panel({ title, children }: { title: string; children: ReactNode }) {
  return (
    <section className="panel">
      <h2>{title}</h2>
      {children}
    </section>
  );
}
