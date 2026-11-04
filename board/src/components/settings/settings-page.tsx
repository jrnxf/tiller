import { prefsStore, updatePrefs } from "../../lib/prefs";
import type { Density, ThemePreference } from "../../lib/prefs";
import { useStore } from "../../lib/store";
import "./settings.css";

const THEMES: { value: ThemePreference; label: string }[] = [
  { value: "system", label: "System" },
  { value: "light", label: "Light" },
  { value: "dark", label: "Dark" },
];

const DENSITIES: { value: Density; label: string }[] = [
  { value: "comfortable", label: "Comfortable" },
  { value: "compact", label: "Compact" },
];

function Choice<Value extends string>({
  label,
  hint,
  options,
  value,
  onChange,
}: {
  label: string;
  hint: string;
  options: { value: Value; label: string }[];
  value: Value;
  onChange: (value: Value) => void;
}) {
  return (
    <div className="set-row">
      <div className="set-text">
        <h3>{label}</h3>
        <p>{hint}</p>
      </div>
      <div className="seg" role="group" aria-label={label}>
        {options.map((option) => (
          <button
            key={option.value}
            type="button"
            aria-pressed={value === option.value}
            onClick={() => onChange(option.value)}
          >
            {option.label}
          </button>
        ))}
      </div>
    </div>
  );
}

export function SettingsPage() {
  const prefs = useStore(prefsStore);
  return (
    <div className="settings-page" data-testid="settings">
      <section className="set-panel" aria-label="Appearance">
        <Choice
          label="Theme"
          hint="Follow the system, or stay light or dark"
          options={THEMES}
          value={prefs.theme}
          onChange={(theme) => updatePrefs({ theme })}
        />
        <Choice
          label="Density"
          hint="How much room rows and cards take"
          options={DENSITIES}
          value={prefs.density}
          onChange={(density) => updatePrefs({ density })}
        />
      </section>
    </div>
  );
}
