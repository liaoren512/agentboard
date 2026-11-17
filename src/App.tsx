import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import Board from "./Board";
import type { SessionSnapshot } from "./types";
import "./styles.css";

export default function App() {
  const [sessions, setSessions] = useState<SessionSnapshot[]>([]);
  useEffect(() => {
    invoke<SessionSnapshot[]>("get_sessions").then(setSessions);
    const un = listen<SessionSnapshot[]>("sessions-changed", (e) => setSessions(e.payload));
    return () => { un.then((f) => f()); };
  }, []);
  return <Board sessions={sessions} />;
}
