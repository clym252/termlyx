import { useEffect, useState } from "react";
import { api } from "./api";
import { pressBack } from "./app/back";
import { Gate } from "./app/Gate";
import type { BenchConfig } from "./dev/bench";
import { DevTerminal, loadBenchConfig } from "./dev/DevTerminal";
import { IS_ANDROID } from "./platform";
import { PromptHost } from "./prompts/PromptHost";
import { QuitGuard } from "./workspace/QuitGuard";

export default function App() {
  // DEV only: a bench run goes straight to the terminal once the vault is
  // unlocked (host keys are checked against it; a locked vault rejects them).
  const [bench, setBench] = useState<BenchConfig | null | "checking">(
    import.meta.env.DEV && import.meta.env.VITE_TETHER_API !== "mock" ? "checking" : null,
  );

  useEffect(() => {
    if (bench !== "checking") return;
    let live = true;
    void loadBenchConfig().then((cfg) => {
      if (live) setBench(cfg);
    });
    return () => {
      live = false;
    };
  }, [bench]);

  // Android's Back never closes the app: with no page up (setup, unlock) it goes to the background.
  useEffect(() => {
    if (!IS_ANDROID) return;
    let off: (() => void) | undefined;
    let live = true;
    void api.onBackButton(() => pressBack(() => void api.moveToBackground())).then((un) => (live ? (off = un) : un()));
    return () => {
      live = false;
      off?.();
    };
  }, []);

  return (
    <div className="app">
      {bench === "checking" ? null : bench ? <Gate unlockedView={<DevTerminal bench={bench} />} /> : <Gate />}
      <PromptHost />
      <QuitGuard />
    </div>
  );
}
