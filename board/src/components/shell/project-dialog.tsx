import { useEffect, useRef } from "react";
import type { ReactNode, RefObject } from "react";
import { createPortal } from "react-dom";

import type { ProjectAgent } from "../../lib/api-types";
import { ReadError } from "../../lib/queries";

// `fleet` is never deleted or archived, so it has neither control.
export const FLEET_PROJECT = "fleet";

// "builder on alpha-42", or "orchestrator".
function agentName(agent: ProjectAgent): string {
  return agent.card ? `${agent.role} on ${agent.card}` : agent.role;
}

// "1 agent", "3 agents".
export function agentCount(agents: ProjectAgent[]): string {
  return agents.length === 1 ? "1 agent" : `${agents.length} agents`;
}

// The running agents a project delete or archive stops, the orchestrator
// first, as a section of the dialog's card.
export function AgentList({ agents, title }: { agents: ProjectAgent[]; title: string }) {
  return (
    <section className="pdialog-section" aria-label={title}>
      <h3>{title}</h3>
      <ul>
        {agents.map((agent) => (
          <li key={agent.session ?? agent.run ?? agentName(agent)}>
            <span>{agentName(agent)}</span>
          </li>
        ))}
      </ul>
    </section>
  );
}

// Why a preview could not be read, as the dialog says it.
export function readFailure(error: Error | null, fallback: string): string | null {
  if (error instanceof ReadError) return error.reason ?? error.message;
  return error ? fallback : null;
}

// A project dialog on the page itself, like the stage switch's: the backdrop
// and Escape close it unless a send is in flight, and Tab stays on its own
// controls. `initial` takes focus when it opens.
export function ProjectDialog({
  name,
  className,
  pending,
  onClose,
  initial,
  children,
}: {
  name: string;
  className?: string;
  pending: boolean;
  onClose: () => void;
  initial: RefObject<HTMLElement | null>;
  children: ReactNode;
}) {
  const dialog = useRef<HTMLDivElement>(null);
  useEffect(() => {
    initial.current?.focus();
  }, [initial]);

  return createPortal(
    <div
      className="dialog-backdrop"
      onMouseDown={() => {
        if (!pending) onClose();
      }}
    >
      <div
        ref={dialog}
        className={className ? `dialog ${className}` : "dialog"}
        role="dialog"
        aria-modal="true"
        aria-labelledby={`${name}-dialog-title`}
        aria-describedby={`${name}-dialog-body`}
        onMouseDown={(event) => event.stopPropagation()}
        onKeyDown={(event) => {
          if (event.key === "Escape") {
            event.preventDefault();
            event.stopPropagation();
            if (!pending) onClose();
          } else if (event.key === "Tab") {
            // Keep focus on the dialog's own controls that can take it.
            const stops = [
              ...(dialog.current?.querySelectorAll<HTMLElement>(
                "input, select, textarea, button, a[href]",
              ) ?? []),
            ].filter((element) => !(element as HTMLButtonElement).disabled);
            const [first, last] = [stops[0], stops.at(-1)];
            if (!first) {
              // Everything is disabled while the action is sent: focus stays.
              event.preventDefault();
            } else if (event.shiftKey && document.activeElement === first) {
              event.preventDefault();
              last?.focus();
            } else if (!event.shiftKey && document.activeElement === last) {
              event.preventDefault();
              first?.focus();
            }
          }
        }}
      >
        {children}
      </div>
    </div>,
    document.body,
  );
}
