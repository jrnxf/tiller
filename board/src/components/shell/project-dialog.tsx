import type { ReactNode, RefObject } from "react";

import type { ProjectAgent } from "../../lib/api-types";
import { ReadError } from "../../lib/queries";
import { Modal } from "../ui/modal";

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
// and Escape close it unless a send is in flight. `initial` takes focus when
// it opens.
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
  return (
    <Modal
      className={className ? `dialog ${className}` : "dialog"}
      labelledBy={`${name}-dialog-title`}
      describedBy={`${name}-dialog-body`}
      dismissable={!pending}
      onClose={onClose}
      initialFocus={initial}
    >
      {children}
    </Modal>
  );
}
