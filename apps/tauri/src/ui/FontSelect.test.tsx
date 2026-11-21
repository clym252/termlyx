import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

vi.mock("./fonts", async (orig) => {
  const m = await orig<typeof import("./fonts")>();
  return { ...m, installedFonts: () => ["JetBrains Mono", "Menlo"] };
});
import { FontSelect } from "./FontSelect";

afterEach(cleanup);
const opts = () => [...(screen.getByRole("combobox") as HTMLSelectElement).options];

describe("FontSelect", () => {
  it("lists a typed stack as a custom option that round-trips", () => {
    const old = 'Menlo, Monaco, "Cascadia Mono", Consolas, "Courier New", monospace';
    render(<FontSelect value={old} onChange={() => {}} />);
    const sel = screen.getByRole("combobox") as HTMLSelectElement;
    expect(sel.value).toBe(old);
    expect(opts()[0].textContent).toBe("Menlo (custom)");
    expect(opts()[0].value).toBe(old);
  });
  it("offers an inherit option first", () => {
    render(<FontSelect value="" onChange={vi.fn()} inherit={{ label: "Same as app (JetBrains Mono)" }} />);
    expect(opts()[0].value).toBe("");
    expect(opts().some((o) => o.textContent?.includes("custom"))).toBe(false);
  });
  it("reports the chosen stack", () => {
    const onChange = vi.fn();
    render(<FontSelect value='"JetBrains Mono", monospace' onChange={onChange} />);
    fireEvent.change(screen.getByRole("combobox"), { target: { value: '"Menlo", monospace' } });
    expect(onChange).toHaveBeenCalledWith('"Menlo", monospace');
  });
});
