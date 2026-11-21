import React from "react";
import ReactDOM from "react-dom/client";
import "@fontsource/jetbrains-mono/latin-400.css";
import "@fontsource/jetbrains-mono/latin-700.css";
import App from "./App";
import "./App.css";
import "./ui.css";
import { api } from "./api";
import { getTerminalSettings, subscribeTerminalSettings } from "./settings/store";
import { applyUiTheme, getTheme } from "./themes";
import { trackVisualViewport } from "./viewport";
import { sessionsReady } from "./workspace/startup";

// A reload leaves the previous page's sessions running in Rust: close them
// before anything opens a new one (useShell waits for this).
void sessionsReady(api);

trackVisualViewport();

// The chosen theme colours the app (and, through the panes, the terminals).
applyUiTheme(getTheme(getTerminalSettings().themeId));
subscribeTerminalSettings(() => applyUiTheme(getTheme(getTerminalSettings().themeId)));

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
