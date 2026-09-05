import { NavLink, Route, Routes } from "react-router-dom";
import Overview from "./pages/Overview";
import Incidents from "./pages/Incidents";
import IncidentDetail from "./pages/IncidentDetail";
import Events from "./pages/Events";
import Sources from "./pages/Sources";

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
