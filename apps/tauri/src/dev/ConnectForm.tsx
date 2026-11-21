import { useState, type FormEvent } from "react";
import type { ConnectInput } from "../terminal/useShell";

/** Dev-only scaffolding for Phase 0; replaced by the real connection UI in Phase 1. */
export function ConnectForm({ onConnect }: { onConnect: (p: ConnectInput) => void }) {
  const [host, setHost] = useState("127.0.0.1");
  const [port, setPort] = useState("2222");
  const [username, setUsername] = useState("tether");
  const [password, setPassword] = useState("");

  const submit = (e: FormEvent) => {
    e.preventDefault();
    onConnect({ host, port: Number(port), username, password });
  };

  return (
    <form className="connect-form" onSubmit={submit}>
      <input aria-label="host" value={host} onChange={(e) => setHost(e.target.value)} />
      <input aria-label="port" value={port} onChange={(e) => setPort(e.target.value)} />
      <input
        aria-label="username"
        value={username}
        onChange={(e) => setUsername(e.target.value)}
      />
      <input
        aria-label="password"
        type="password"
        value={password}
        onChange={(e) => setPassword(e.target.value)}
      />
      <button type="submit">Connect</button>
    </form>
  );
}
