import { useSuspenseQuery } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";

import { attentionQuery } from "../../lib/queries";
import { MachinesSection } from "./machines-section";
import { ProjectsSection } from "./projects-section";
import { QuotaFooter } from "./quota-footer";

const NAV = [
  { to: "/", label: "Board" },
  { to: "/agents", label: "Agents" },
  { to: "/inbox", label: "Inbox" },
  { to: "/memory", label: "Memory" },
  { to: "/health", label: "Health" },
] as const;

export function Sidebar({
  activeHost,
  onNavigate,
}: {
  activeHost: string | undefined;
  onNavigate: () => void;
}) {
  const { data: attention } = useSuspenseQuery(attentionQuery());
  const waiting = attention.items.length + attention.permission_prompts.length;

  return (
    <aside className="sidebar" aria-label="Sidebar">
      <div className="brand">
        <i aria-hidden="true" />
        Tiller
      </div>
      <nav className="sec" aria-label="Screens" onClick={onNavigate}>
        {NAV.map((item) => (
          <Link
            key={item.to}
            to={item.to}
            search={(prev) => prev}
            className="row"
            activeProps={{ className: "row on", "aria-current": "page" }}
            activeOptions={{ exact: true }}
          >
            {item.label}
            {item.to === "/inbox" && waiting > 0 ? <span className="n">{waiting}</span> : null}
          </Link>
        ))}
      </nav>
      <ProjectsSection />
      <MachinesSection activeHost={activeHost} />
      <div className="sidebar-foot" onClick={onNavigate}>
        <QuotaFooter />
      </div>
    </aside>
  );
}
