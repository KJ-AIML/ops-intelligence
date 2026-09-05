import { useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { api, formatTime } from "../api";
import type { CreatedSource } from "../api";
import { Empty, ErrorNotice, Loading } from "../components";

export default function Sources() {
  const queryClient = useQueryClient();
  const [name, setName] = useState("");
  const [created, setCreated] = useState<CreatedSource | null>(null);

  const sources = useQuery({ queryKey: ["sources"], queryFn: api.sources });

  const refresh = () => void queryClient.invalidateQueries({ queryKey: ["sources"] });

  const create = useMutation({
    mutationFn: (n: string) => api.createSource(n),
    onSuccess: (source) => {
      // The token is readable exactly once, right here. Hold it in component
      // state so the engineer can copy it before it becomes unrecoverable.
      setCreated(source);
      setName("");
      refresh();
    },
  });

  const toggle = useMutation({
    mutationFn: ({ id, enabled }: { id: string; enabled: boolean }) =>
      api.setSourceEnabled(id, enabled),
    onSuccess: refresh,
  });

  return (
    <>
      <h1>Sources</h1>
      <p className="subtitle">
        Every source writes raw signals through the same boundary, so adding one changes nothing
        downstream.
      </p>

      <div className="toolbar">
        <input
          placeholder="new webhook source name"
          value={name}
          onChange={(e) => setName(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && name.trim()) create.mutate(name.trim());
          }}
        />
        <button
          className="primary"
          onClick={() => create.mutate(name.trim())}
          disabled={!name.trim() || create.isPending}
        >
          Add webhook source
        </button>
      </div>

      {create.error && <ErrorNotice error={create.error} />}

      {created && (
        <div className="panel section">
          <h2>Ingestion endpoint for {created.name}</h2>
          <p className="small" style={{ marginTop: 0 }}>
            Copy this now. The token is shown once and is not recoverable afterwards — it is the
            source's credential and identifies the tenant.
          </p>
          <pre>{created.ingest_url}</pre>
          <div className="toolbar" style={{ marginTop: 12, marginBottom: 0 }}>
            <button onClick={() => void navigator.clipboard?.writeText(created.ingest_url)}>
              Copy URL
            </button>
            <button onClick={() => setCreated(null)}>Done</button>
          </div>
        </div>
      )}

      {sources.isPending ? (
        <Loading what="sources" />
      ) : sources.isError ? (
        <ErrorNotice error={sources.error} />
      ) : sources.data.length === 0 ? (
        <Empty>No sources configured yet.</Empty>
      ) : (
        <div className="panel" style={{ padding: 0 }}>
          <table>
            <thead>
              <tr>
                <th>Source</th>
                <th>Type</th>
                <th>Ingestion</th>
                <th>Last seen</th>
                <th>Status</th>
                <th />
              </tr>
            </thead>
            <tbody>
              {sources.data.map((source) => (
                <tr key={source.id}>
                  <td>{source.name}</td>
                  <td className="mono small muted">{source.source_type}</td>
                  <td className="small muted">
                    {source.has_ingest_token ? "webhook token set" : "offline import"}
                  </td>
                  <td className="small muted nowrap">
                    {source.last_seen_at ? formatTime(source.last_seen_at) : <span className="dim">never</span>}
                  </td>
                  <td>
                    <span className={`badge ${source.enabled ? "st-recovered" : "st-resolved"}`}>
                      {source.enabled ? "enabled" : "disabled"}
                    </span>
                  </td>
                  <td style={{ textAlign: "right" }}>
                    <button
                      onClick={() => toggle.mutate({ id: source.id, enabled: !source.enabled })}
                      disabled={toggle.isPending}
                    >
                      {source.enabled ? "Disable" : "Enable"}
                    </button>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </>
  );
}
