import { familyOf, installedFonts, stackFor } from "./fonts";

/** Pick a terminal font from the ones on this device. A stored stack that is not in the list stays selectable as "(custom)". */
export function FontSelect(p: {
  id?: string;
  value: string;
  onChange: (stack: string) => void;
  inherit?: { label: string };
  className?: string;
  "aria-describedby"?: string;
  "aria-invalid"?: boolean;
}) {
  const fonts = installedFonts();
  const known = (p.inherit && p.value === "") || fonts.some((f) => stackFor(f) === p.value);
  return (
    <select
      id={p.id}
      className={p.className}
      aria-describedby={p["aria-describedby"]}
      aria-invalid={p["aria-invalid"]}
      value={p.value} onChange={(e) => p.onChange(e.target.value)}>
      {p.inherit && <option value="">{p.inherit.label}</option>}
      {!known && <option value={p.value}>{familyOf(p.value)} (custom)</option>}
      {fonts.map((f) => (
        <option key={f} value={stackFor(f)}>
          {f}
        </option>
      ))}
    </select>
  );
}
