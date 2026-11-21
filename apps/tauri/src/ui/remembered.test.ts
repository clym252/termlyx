import { renderHook } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { useRemembered } from "./remembered";

const rec = (id: string, name: string) => ({ id, name });
type Props = { id: string | null; found?: ReturnType<typeof rec> | undefined };

describe("useRemembered", () => {
  it("returns the found record, and the last one seen once it is gone", () => {
    const { result, rerender } = renderHook(
      ({ id, found }: Props) => useRemembered(id, found),
      { initialProps: { id: "a", found: rec("a", "first") } as Props },
    );
    expect(result.current).toEqual(rec("a", "first"));
    rerender({ id: "a", found: rec("a", "second") });
    expect(result.current).toEqual(rec("a", "second"));
    rerender({ id: "a", found: undefined });
    expect(result.current).toEqual(rec("a", "second"));
  });

  it("forgets it when another record, or none, is asked for", () => {
    const { result, rerender } = renderHook(
      ({ id, found }: Props) => useRemembered(id, found),
      { initialProps: { id: "a", found: rec("a", "first") } as Props },
    );
    rerender({ id: "b", found: undefined });
    expect(result.current).toBeUndefined();
    rerender({ id: "a", found: undefined });
    expect(result.current).toBeUndefined();
    rerender({ id: null, found: undefined });
    expect(result.current).toBeUndefined();
  });
});
