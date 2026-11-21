import { SHORTCUT_LABELS } from "../workspace/shortcuts";

/** Opens a hidden sidebar as a drawer over the page: the first thing in the terminal tab bar (pages show the phone's bar instead). */
export function ShowSidebarButton({ onShow }: { onShow: () => void }) {
  return (
    <button
      type="button"
      className="show-sidebar"
      aria-label="Show sidebar"
      aria-haspopup="dialog"
      title={`Show sidebar (${SHORTCUT_LABELS.sidebar})`}
      onClick={onShow}
    >
      ☰
    </button>
  );
}
