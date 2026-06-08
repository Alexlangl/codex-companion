import type { ReactNode } from "react";

export function Button({
  children,
  disabled,
  iconOnly,
  label,
  onClick,
  variant = "default",
  type = "button",
}: {
  children: ReactNode;
  disabled?: boolean;
  iconOnly?: boolean;
  label?: string;
  onClick?: () => void;
  variant?: "default" | "secondary" | "danger" | "ghost";
  type?: "button" | "submit";
}) {
  return (
    <button
      aria-label={label}
      className={`button button-${variant}${iconOnly ? " button-icon-only" : ""}`}
      disabled={disabled}
      onClick={onClick}
      title={label}
      type={type}
    >
      {children}
    </button>
  );
}

export function IconButton({
  children,
  label,
  disabled,
  onClick,
}: {
  children: ReactNode;
  label: string;
  disabled?: boolean;
  onClick?: () => void;
}) {
  return (
    <button
      aria-label={label}
      className="icon-button"
      disabled={disabled}
      onClick={onClick}
      title={label}
      type="button"
    >
      {children}
    </button>
  );
}

export function Field({
  children,
  label,
}: {
  children: ReactNode;
  label: string;
}) {
  return (
    <label className="field">
      <span>{label}</span>
      {children}
    </label>
  );
}

export function Panel({
  children,
  title,
  eyebrow,
}: {
  children: ReactNode;
  title: string;
  eyebrow?: string;
}) {
  return (
    <section className="panel">
      {eyebrow ? <div className="panel-eyebrow">{eyebrow}</div> : null}
      <h2>{title}</h2>
      {children}
    </section>
  );
}

export type BadgeTone = "neutral" | "ok" | "warn" | "danger" | "info" | "accent";

export function Badge({ children, tone = "neutral" }: { children: ReactNode; tone?: BadgeTone }) {
  return <span className={`badge badge-${tone}`}>{children}</span>;
}
