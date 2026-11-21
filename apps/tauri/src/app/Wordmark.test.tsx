import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import { Wordmark } from "./Wordmark";

afterEach(cleanup);

describe("Wordmark", () => {
  it("shows the mark and spaced caps name", () => {
    render(<Wordmark />);
    expect(screen.getByRole("img", { name: "Termlyx" })).not.toBeNull();
    expect(screen.getByText("TERMLYX")).not.toBeNull();
  });
  it("compact is the mark only, still named", () => {
    render(<Wordmark compact />);
    expect(screen.getByRole("img", { name: "Termlyx" })).not.toBeNull();
    expect(screen.queryByText("TERMLYX")).toBeNull();
  });
});
