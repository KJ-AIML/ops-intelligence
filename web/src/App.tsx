import { useEffect, useState } from "react";
import { NavLink, Route, Routes } from "react-router-dom";
import { setApiToken, TOKEN_REQUIRED_EVENT } from "./api";
import Overview from "./pages/Overview";
import Incidents from "./pages/Incidents";
import IncidentDetail from "./pages/IncidentDetail";
import Events from "./pages/Events";
import Sources from "./pages/Sources";

/** Shown only after the server has answered 401. Saving reloads so every query
 *  refetches with the header; crude, and exactly enough for a pilot. */
function TokenBar() {
  const [needed, setNeeded] = useState(false);
  const [value, setValue] = useState("");
  useEffect(() => {
    const show = () => setNeeded(true);
    window.addEventListener(TOKEN_REQUIRED_EVENT, show);
    return () => window.removeEventListener(TOKEN_REQUIRED_EVENT, show);
  }, []);
  if (!needed) return null;
  const save = () => {
    if (!value.trim()) return;
    setApiToken(value);
    window.location.reload();
  };
  return (
    <div className="notice error" style={{ display: "flex", gap: "0.5rem", alignItems: "center" }}>
      <span>This server requires an API token.</span>
      <input
        type="password"
        placeholder="paste API_TOKEN"
        value={value}
        onChange={(e) => setValue(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter") save();
        }}
      />
      <button className="primary" onClick={save} disabled={!value.trim()}>
        Save
      </button>
    </div>
  );
}

export default function App() {
  return (
    <div className="app">
      <header className="topbar">
        <div className="brand">
          Operations Intelligence<span>pilot</span>
        </div>
        <nav className="nav">
          <NavLink to="/" end>
            Overview
          </NavLink>
          <NavLink to="/incidents">Incidents</NavLink>
          <NavLink to="/events">Events</NavLink>
          <NavLink to="/sources">Sources</NavLink>
        </nav>
      </header>

      <TokenBar />

      <Routes>
        <Route path="/" element={<Overview />} />
        <Route path="/incidents" element={<Incidents />} />
        <Route path="/incidents/:id" element={<IncidentDetail />} />
        <Route path="/events" element={<Events />} />
        <Route path="/sources" element={<Sources />} />
      </Routes>
    </div>
  );
}
