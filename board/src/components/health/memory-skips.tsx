import { useQuery } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";

import type { MemoryDocSummary } from "../../lib/api-types";
import { memoryQuery } from "../../lib/queries";
import { WarnIcon } from "../board/icons";
import { sentence } from "../memory/parts";

const FLEET = "fleet";

function label(doc: MemoryDocSummary): string {
  return doc.project_id === FLEET
    ? "the shared memory document"
    : `the ${doc.name} memory document`;
}

// The memory documents a launch goes on without (memory.md section 9, step
// 4), each named with why and a way to Memory. Nothing shows while every
// document loads.
export function MemorySkips() {
  const read = useQuery(memoryQuery());
  const skipped = (read.data?.docs ?? []).filter((doc) => doc.skipped !== null);
  if (skipped.length === 0) return null;
  return (
    <section className="cpanel learning" aria-labelledby="memory-skips-title">
      <div className="ch">
        <div>
          <h3 id="memory-skips-title">Memory</h3>
          <p className="cs">Documents launches go on without.</p>
        </div>
      </div>
      <ul className="ln-list">
        {skipped.map((doc) => (
          <li key={doc.project_id} data-kind="skipped">
            <span className="si bad">
              <WarnIcon />
            </span>
            <span className="t">Launches skip {label(doc)}</span>
            <span className="m">
              {sentence(doc.skipped ?? "")} An edit in Memory approves its text, and launches load
              it again.
            </span>
            <span className="acts">
              <Link className="btn" to="/memory" search={{ doc: doc.project_id }}>
                Open in Memory
              </Link>
            </span>
          </li>
        ))}
      </ul>
    </section>
  );
}
