import type { ReactNode } from "react";

export interface SegmentedOption<Value extends string> {
  value: Value;
  label: string;
  icon?: ReactNode;
}

// The board's tabs: one button per choice, the chosen one pressed. Settings
// uses it for the theme and the sidebar for the project view.
export function Segmented<Value extends string>({
  options,
  value,
  onChange,
  label,
  labelledBy,
  className,
  disabled,
}: {
  options: SegmentedOption<Value>[];
  value: Value;
  onChange: (value: Value) => void;
  label?: string;
  labelledBy?: string;
  className?: string;
  disabled?: boolean;
}) {
  return (
    <div
      className={`seg${className ? ` ${className}` : ""}`}
      role="group"
      aria-label={label}
      aria-labelledby={labelledBy}
    >
      {options.map((option) => (
        <button
          key={option.value}
          type="button"
          aria-pressed={value === option.value}
          disabled={disabled}
          onClick={() => onChange(option.value)}
        >
          {option.icon}
          {option.label}
        </button>
      ))}
    </div>
  );
}
