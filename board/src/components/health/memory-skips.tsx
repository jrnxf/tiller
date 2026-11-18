import { useQuery } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";

import { memoryQuery } from "../../lib/queries";
import { WarnIcon } from "../board/icons";

// kb on this host, when the fleet cannot load from it (memory.md section 4),
// with a way to Memory. Nothing shows while kb works.
export function MemorySkips() {
  const read = useQuery(memoryQuery());
  const kb = read.data?.kb;
  if (!kb || kb.state === "ok") return null;
  return (
    <section className="cpanel learning" aria-labelledby="memory-skips-title">
      <div className="ch">
        <div>
          <h3 id="memory-skips-title">Memory</h3>
          <p className="cs">What the fleet loads from kb.</p>
        </div>
      </div>
      <ul className="ln-list">
        <li data-kind="skipped">
          <span className="si bad">
            <WarnIcon />
          </span>
          <span className="t">
            {kb.state === "missing"
              ? "tiller cannot find kb on this host"
              : "tiller cannot read kb on this host"}
          </span>
          <span className="m">The fleet loads no notes on this host until kb works again.</span>
          <span className="acts">
            <Link className="btn" to="/memory" search={{ tab: "live" }}>
              Open in Memory
            </Link>
          </span>
        </li>
      </ul>
    </section>
  );
}
