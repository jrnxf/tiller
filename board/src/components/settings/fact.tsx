import type { ReactNode } from "react";

import "./settings.css";

// A row that shows a value and has no control.
export function Fact({
  label,
  hint,
  children,
}: {
  label: string;
  hint: ReactNode;
  children: ReactNode;
}) {
  return (
    <div className="set-row">
      <div className="set-text">
        <h2>{label}</h2>
        <p>{hint}</p>
      </div>
      <div className="set-value">{children}</div>
    </div>
  );
}
