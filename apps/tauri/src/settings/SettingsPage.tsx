import { FORM_FACTOR, IS_MOBILE } from "../platform";
import { AboutSettings } from "./AboutSettings";
import { KeepAliveSetting } from "./KeepAliveSetting";
import { SecuritySettings } from "./SecuritySettings";
import { SyncSettings } from "./SyncSettings";
import { SidebarSetting } from "./SidebarSetting";
import { TerminalSettings } from "./TerminalSettings";

/** Settings: terminal appearance (per device), sync, security and (iOS) background keep-alive. */
export function SettingsPage() {
  return (
    <section className="page settings-page" aria-labelledby="settings-title">
      <header className="page-head">
        <h1 id="settings-title">Settings</h1>
      </header>
      <TerminalSettings />
      {FORM_FACTOR !== "phone" && <SidebarSetting />}
      <SyncSettings />
      <SecuritySettings />
      {IS_MOBILE && <KeepAliveSetting />}
      <AboutSettings />
    </section>
  );
}
