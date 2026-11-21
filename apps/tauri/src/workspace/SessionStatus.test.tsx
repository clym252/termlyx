import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { ShellState } from "../terminal/useShell";
import { SessionStatus, type SessionHeader } from "./SessionStatus";

afterEach(cleanup);

const noop = () => {};
const HEADER: SessionHeader = {
  label: "web-1",
  address: "deploy@web1.example:2200",
  badge: { text: "W1", colorIndex: 2 },
};

const connecting = (step: ShellState["step"]): ShellState => ({ status: "connecting", step });
const failed = (step: ShellState["step"], error: string): ShellState => ({ status: "error", step, error });

function shown(state: ShellState, props: Partial<Parameters<typeof SessionStatus>[0]> = {}) {
  return render(
    <SessionStatus
      state={state}
      header={HEADER}
      onReconnect={noop}
      onCancel={noop}
      onClose={noop}
      {...props}
    />,
  );
}

describe("SessionStatus", () => {
  it("connecting_shows_the_host_and_four_steps", () => {
    const { container } = shown(connecting("auth"));
    const card = screen.getByRole("status");
    expect(within(card).getAllByRole("listitem")).toHaveLength(4);
    expect(within(card).getByText("Connecting…")).toBeTruthy();
    expect(within(card).getByText("web-1")).toBeTruthy();
    expect(within(card).getByText("deploy@web1.example:2200")).toBeTruthy();
    const badge = container.querySelector(".host-badge") as HTMLElement;
    expect(badge.textContent).toBe("W1");
    expect(badge.style.background).toBe("var(--badge-3-bg)");
    expect(badge.style.color).toBe("var(--badge-3-fg)");
    const marks = within(card)
      .getAllByRole("listitem")
      .map((li) => [li.textContent, li.getAttribute("data-mark")]);
    expect(marks).toEqual([
      ["Reaching host", "done"],
      ["Checking host key", "done"],
      ["Signing in", "current"],
      ["Opening shell", "todo"],
    ]);
  });

  it("waiting_on_another_tab_shows_all_steps_not_yet_reached", () => {
    shown(connecting(null));
    const marks = screen.getAllByRole("listitem").map((li) => li.getAttribute("data-mark"));
    expect(marks).toEqual(["todo", "todo", "todo", "todo"]);
  });

  it("cancel_calls_onCancel", () => {
    const onCancel = vi.fn();
    shown(connecting("tcp"), { onCancel });
    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
    expect(onCancel).toHaveBeenCalledTimes(1);
  });

  it("says_when_the_host_key_dialog_is_waiting_for_the_user", () => {
    const { rerender } = shown(connecting("hostKey"));
    expect(screen.getByText("Checking host key")).toBeTruthy();
    rerender(
      <SessionStatus
        state={connecting("hostKey")}
        header={HEADER}
        waitingForYou
        onReconnect={noop}
        onCancel={noop}
        onClose={noop}
      />,
    );
    expect(screen.getByText("Checking host key — waiting for you")).toBeTruthy();
  });

  it("failed_marks_the_step_with_the_reason_and_leaves_later_steps_hollow", () => {
    shown(failed("auth", "password rejected"));
    const card = screen.getByRole("alert");
    expect(within(card).getByText("Could not connect")).toBeTruthy();
    expect(within(card).getByText("Signing in — password rejected")).toBeTruthy();
    const marks = within(card)
      .getAllByRole("listitem")
      .map((li) => li.getAttribute("data-mark"));
    expect(marks).toEqual(["done", "done", "failed", "todo"]);
  });

  it("failed_puts_the_cores_timeout_in_words", () => {
    shown(failed("tcp", "timed out"));
    expect(screen.getByText("Reaching host — timed out after 15 s")).toBeTruthy();
  });

  it("failed_takes_focus_on_retry_when_asked", () => {
    const state = failed("auth", "password rejected");
    const { rerender } = shown(state);
    expect(document.activeElement).toBe(document.body);
    rerender(
      <SessionStatus state={state} header={HEADER} takeFocus onReconnect={noop} onCancel={noop} onClose={noop} />,
    );
    expect(document.activeElement).toBe(screen.getByRole("button", { name: "Retry" }));
  });

  it("failed_with_no_step_shows_the_reason_alone", () => {
    shown(failed(null, "That shell is no longer available."), { header: null });
    expect(screen.queryAllByRole("listitem")).toHaveLength(0);
    expect(screen.getByRole("alert").textContent).toContain("That shell is no longer available.");
  });

  it("lost_gives_the_reason_with_reconnect_and_close_but_no_steps", () => {
    const onReconnect = vi.fn();
    const onClose = vi.fn();
    shown({ status: "lost", step: "shell", error: "reset by peer" }, { onReconnect, onClose, takeFocus: true });
    const card = screen.getByRole("alert");
    expect(card.textContent).toContain("Connection lost");
    expect(card.textContent).toContain("reset by peer");
    expect(screen.queryAllByRole("listitem")).toHaveLength(0);
    expect(document.activeElement).toBe(screen.getByRole("button", { name: "Reconnect" }));
    fireEvent.click(screen.getByRole("button", { name: "Reconnect" }));
    fireEvent.click(screen.getByRole("button", { name: "Close" }));
    expect(onReconnect).toHaveBeenCalledTimes(1);
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it("ended_has_no_steps_and_only_close", () => {
    const onClose = vi.fn();
    shown({ status: "ended", step: "shell", exitStatus: 3 }, { onClose });
    expect(screen.getByRole("status").textContent).toContain("Session ended (exit status 3)");
    expect(screen.queryAllByRole("listitem")).toHaveLength(0);
    expect(screen.getAllByRole("button").map((b) => b.textContent)).toEqual(["Close"]);
    fireEvent.click(screen.getByRole("button", { name: "Close" }));
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it("local_and_direct_sessions_have_no_host_header", () => {
    const { container } = shown(connecting("shell"), { header: null });
    expect(container.querySelector(".host-badge")).toBeNull();
    expect(screen.getAllByRole("listitem")).toHaveLength(4);
  });

  it("renders_nothing_while_the_session_is_open", () => {
    const { container } = shown({ status: "open", step: "shell" });
    expect(container.firstChild).toBeNull();
  });

  describe("reconnecting", () => {
    const lostState: ShellState = { status: "lost", step: "shell", error: "reset by peer" };

    it("waiting_shows_the_attempt_the_countdown_and_both_buttons", () => {
      const onReconnect = vi.fn();
      const onStopReconnect = vi.fn();
      shown(lostState, {
        reconnect: { kind: "waiting", attempt: 2, dueAt: 10_000 },
        now: 7_200,
        onReconnect,
        onStopReconnect,
      });
      const card = screen.getByRole("status");
      expect(within(card).getByText("Reconnecting…")).toBeTruthy();
      expect(within(card).getByText("attempt 2 of 5")).toBeTruthy();
      expect(within(card).getByText("next try in 3s")).toBeTruthy();
      // The countdown ticks every second and must not be announced each time; the attempt line is.
      expect(within(card).getByText("next try in 3s").getAttribute("aria-live")).toBe("off");
      expect(within(card).getByText("attempt 2 of 5").getAttribute("aria-live")).toBeNull();
      // Between attempts the steps are hollow; the reason the connection went is on the card.
      const marks = within(card)
        .getAllByRole("listitem")
        .map((li) => li.getAttribute("data-mark"));
      expect(marks).toEqual(["todo", "todo", "todo", "todo"]);
      expect(card.textContent).toContain("reset by peer");
      fireEvent.click(screen.getByRole("button", { name: "Reconnect now" }));
      fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
      expect(onReconnect).toHaveBeenCalledTimes(1);
      expect(onStopReconnect).toHaveBeenCalledTimes(1);
    });

    it("trying_shows_the_steps_of_the_attempt", () => {
      shown(connecting("auth"), { reconnect: { kind: "trying", attempt: 3 } });
      const card = screen.getByRole("status");
      expect(within(card).getByText("Reconnecting…")).toBeTruthy();
      expect(within(card).getByText("attempt 3 of 5")).toBeTruthy();
      expect(within(card).queryByText(/next try/)).toBeNull();
      const marks = within(card)
        .getAllByRole("listitem")
        .map((li) => li.getAttribute("data-mark"));
      expect(marks).toEqual(["done", "done", "current", "todo"]);
      expect((screen.getByRole("button", { name: "Reconnect now" }) as HTMLButtonElement).disabled).toBe(true);
    });

    it("a_failed_attempt_between_tries_names_the_failed_step", () => {
      shown(failed("tcp", "timed out"), { reconnect: { kind: "waiting", attempt: 3, dueAt: 5000 }, now: 1000 });
      const card = screen.getByRole("status");
      expect(within(card).getByText("Reaching host — timed out after 15 s")).toBeTruthy();
      expect(within(card).getByText("next try in 4s")).toBeTruthy();
    });

    it("a_stale_session_state_does_not_show_as_progress_on_a_new_attempt", () => {
      shown(lostState, { reconnect: { kind: "trying", attempt: 1 } });
      const marks = screen.getAllByRole("listitem").map((li) => li.getAttribute("data-mark"));
      expect(marks).toEqual(["todo", "todo", "todo", "todo"]);
    });

    it.each(["gaveUp", "manual"] as const)("%s_is_the_connection_lost_card_even_after_a_failed_attempt", (kind) => {
      const onReconnect = vi.fn();
      const onClose = vi.fn();
      shown(failed("hostKey", "host key rejected"), { reconnect: { kind }, onReconnect, onClose, takeFocus: true });
      const card = screen.getByRole("alert");
      expect(within(card).getByText("Connection lost")).toBeTruthy();
      expect(card.textContent).toContain("host key rejected");
      expect(screen.queryAllByRole("listitem")).toHaveLength(0);
      expect(document.activeElement).toBe(screen.getByRole("button", { name: "Reconnect" }));
      expect(screen.queryByRole("button", { name: "Retry" })).toBeNull();
      fireEvent.click(screen.getByRole("button", { name: "Reconnect" }));
      fireEvent.click(screen.getByRole("button", { name: "Close" }));
      expect(onReconnect).toHaveBeenCalledTimes(1);
      expect(onClose).toHaveBeenCalledTimes(1);
    });

    it("cancelled_mid_attempt_still_says_connection_lost", () => {
      shown(connecting("tcp"), { reconnect: { kind: "gaveUp" } });
      const card = screen.getByRole("alert");
      expect(within(card).getByText("Connection lost")).toBeTruthy();
      expect(screen.getByRole("button", { name: "Reconnect" })).toBeTruthy();
    });
  });
});
