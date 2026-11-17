import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import Board from "./Board";
import Settings from "./Settings";
import type { SessionSnapshot } from "./types";
import "./styles.css";

export default function App() {
  const [sessions, setSessions] = useState<SessionSnapshot[]>([]);
  const [view, setView] = useState<"board" | "settings">("board");
  useEffect(() => {
    invoke<SessionSnapshot[]>("get_sessions").then(setSessions);
    const un = listen<SessionSnapshot[]>("sessions-changed", (e) => setSessions(e.payload));
    return () => { un.then((f) => f()); };
  }, []);
  if (view === "settings") return <Settings onBack={() => setView("board")} />;
  return (
    <>
      <button className="settings-btn" onClick={() => setView("settings")}>设置</button>
      <Board sessions={sessions} />
    </>
  );
}
