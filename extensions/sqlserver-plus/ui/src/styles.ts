import type { CSSProperties } from "react";

const muted = "var(--text-muted, #8a8f98)";
const primary = "var(--text-primary, inherit)";
const text: CSSProperties = { margin: 0, fontSize: 12, lineHeight: 1.5 };

export const styles = {
  section: { display: "grid", gap: 12, minWidth: 0 },
  field: { display: "grid", gap: 4, minWidth: 0 },
  label: {
    fontSize: 10,
    fontWeight: 600,
    letterSpacing: "0.05em",
    textTransform: "uppercase",
    color: muted,
  },
  control: {
    width: "100%",
    boxSizing: "border-box",
    padding: "8px 12px",
    fontSize: 14,
    color: primary,
    background: "var(--bg-base, transparent)",
    border: "1px solid var(--border-strong, #5c6370)",
    borderRadius: 6,
  },
  invalid: { borderColor: "var(--accent-error, #e5484d)" },
  help: { ...text, color: muted },
  error: { ...text, color: "var(--accent-error, #e5484d)" },
  notice: {
    ...text,
    padding: "8px 12px",
    color: primary,
    border: "1px solid var(--accent-warning, #d4a72c)",
    borderRadius: 6,
  },
  issue: {
    display: "flex",
    flexWrap: "wrap",
    alignItems: "center",
    gap: 8,
    padding: "8px 12px",
    border: "1px solid var(--accent-error, #e5484d)",
    borderRadius: 6,
  },
  issueText: { ...text, flex: "1 1 220px", color: primary },
  button: {
    padding: "4px 10px",
    fontSize: 12,
    color: primary,
    background: "var(--surface-secondary, transparent)",
    border: "1px solid var(--border-default, #5c6370)",
    borderRadius: 6,
    cursor: "pointer",
  },
  fieldset: {
    display: "grid",
    gap: 12,
    minWidth: 0,
    margin: 0,
    padding: 12,
    border: "1px solid var(--border-subtle, #3a3f47)",
    borderRadius: 8,
  },
  legend: { padding: "0 4px", fontSize: 12, fontWeight: 600, color: primary },
  columns: { display: "grid", gap: 12, gridTemplateColumns: "repeat(auto-fit, minmax(200px, 1fr))" },
  list: { ...text, paddingLeft: 18, color: muted },
  code: { fontFamily: "ui-monospace, SFMono-Regular, Menlo, Consolas, monospace", fontSize: 11, color: primary },
} satisfies Record<string, CSSProperties>;
