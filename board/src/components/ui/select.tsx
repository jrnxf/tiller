import { Select as BaseSelect } from "@base-ui/react/select";

export interface SelectOption {
  value: string;
  label: string;
  disabled?: boolean;
}

export interface SelectGroup {
  label: string;
  options: SelectOption[];
  disabled?: boolean;
}

function Check() {
  return (
    <svg viewBox="0 0 16 16" width="14" height="14" fill="none" aria-hidden="true">
      <path
        d="m3.5 8.5 3 3 6-7"
        stroke="currentColor"
        strokeWidth="1.6"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
    </svg>
  );
}

function Chevrons() {
  return (
    <svg viewBox="0 0 16 16" width="14" height="14" fill="none" aria-hidden="true">
      <path
        d="m4.5 6.5 3.5-3 3.5 3M4.5 9.5l3.5 3 3.5-3"
        stroke="currentColor"
        strokeWidth="1.4"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
    </svg>
  );
}

function Item({ option }: { option: SelectOption }) {
  return (
    <BaseSelect.Item className="sel-item" value={option.value} disabled={option.disabled}>
      <BaseSelect.ItemIndicator className="sel-check">
        <Check />
      </BaseSelect.ItemIndicator>
      <BaseSelect.ItemText>{option.label}</BaseSelect.ItemText>
    </BaseSelect.Item>
  );
}

// The board's one select: a button that opens a list, with the keyboard
// behaviour of a native select (type to jump, arrows, Enter, Escape). Pass
// `groups` for a list with headings; `options` otherwise.
export function Select({
  value,
  onValueChange,
  options = [],
  groups,
  id,
  label,
  disabled,
  title,
  className,
}: {
  value: string;
  onValueChange: (value: string) => void;
  options?: SelectOption[];
  groups?: SelectGroup[];
  id?: string;
  label?: string;
  disabled?: boolean;
  title?: string;
  className?: string;
}) {
  const all = groups ? groups.flatMap((group) => group.options) : options;
  return (
    <BaseSelect.Root
      value={value}
      onValueChange={(next) => {
        if (typeof next === "string") onValueChange(next);
      }}
      items={all}
      disabled={disabled}
    >
      <BaseSelect.Trigger
        id={id}
        aria-label={label}
        title={title}
        className={`sel-trigger${className ? ` ${className}` : ""}`}
      >
        <BaseSelect.Value className="sel-value" />
        <BaseSelect.Icon className="sel-icon">
          <Chevrons />
        </BaseSelect.Icon>
      </BaseSelect.Trigger>
      <BaseSelect.Portal>
        <BaseSelect.Positioner className="sel-pos" sideOffset={4} alignItemWithTrigger={false}>
          <BaseSelect.Popup
            className="sel-popup"
            // The list is portalled but React events still reach the parents
            // of the Select: a click or Escape here must not reach a row or a
            // dialog around it.
            onClick={(event) => event.stopPropagation()}
            onKeyDown={(event) => event.stopPropagation()}
          >
            <BaseSelect.List className="sel-list">
              {groups
                ? groups.map((group, index) => (
                    <BaseSelect.Group key={`${index}:${group.label}`} className="sel-group">
                      <BaseSelect.GroupLabel className="sel-group-label">
                        {group.label}
                      </BaseSelect.GroupLabel>
                      {group.options.map((option) => (
                        <Item
                          key={option.value}
                          option={{ ...option, disabled: option.disabled || group.disabled }}
                        />
                      ))}
                    </BaseSelect.Group>
                  ))
                : options.map((option) => <Item key={option.value} option={option} />)}
            </BaseSelect.List>
          </BaseSelect.Popup>
        </BaseSelect.Positioner>
      </BaseSelect.Portal>
    </BaseSelect.Root>
  );
}
