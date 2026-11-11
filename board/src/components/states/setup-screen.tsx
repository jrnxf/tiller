import { useEffect } from "react";

import { SNAPSHOT_PATH } from "../../lib/boot";
import { fetchJson } from "../../lib/queries";
import { useAppearance } from "../shell/shell";

// How often the setup screen asks whether `tiller init` has run.
export const SETUP_POLL_MS = 2_000;

// The daemon runs, but this machine has no state.db yet: a fresh install
// whose service started before anyone set it up. The screen names the
// commands to run and opens the board by itself once the daemon serves it.
export function SetupScreen({ command, onReady }: { command: string; onReady: () => void }) {
  useAppearance();

  useEffect(() => {
    let done = false;
    const timer = setInterval(() => {
      // The snapshot, not health: health says initialized as soon as the
      // file exists, while `tiller init` may still be writing it.
      fetchJson(SNAPSHOT_PATH).then(
        () => {
          if (done) return;
          done = true;
          clearInterval(timer);
          onReady();
        },
        () => {},
      );
    }, SETUP_POLL_MS);
    return () => {
      done = true;
      clearInterval(timer);
    };
  }, [onReady]);

  return (
    <main className="setup" data-testid="setup-screen">
      <div className="setup-card">
        <div className="ws">
          <div className="logo">T</div>
          <div className="name">tiller</div>
        </div>
        <h1>Set up Tiller on this machine</h1>
        <p>
          The daemon is running, but this machine has no Tiller state yet. Run these in a terminal,
          one at a time, in this order. The first hands this machine to Tiller, because{" "}
          <code>tiller init</code> refuses while another system owns it. The second takes a name for
          this machine and how many agents it may run at once:
        </p>
        <ol className="setup-steps">
          {command.split("\n").map((line) => (
            <li key={line}>
              <pre>
                {/* Wraps between words only, never inside a flag. */}
                <code>
                  {line.split(" ").map((word, index) => (
                    <span key={index}>
                      {index > 0 ? " " : null}
                      <span className="word">{word}</span>
                    </span>
                  ))}
                </code>
              </pre>
            </li>
          ))}
        </ol>
        <p className="sub">
          On a work machine, add <code>--boundary work</code>.
        </p>
        <p className="wait" role="status">
          <i aria-hidden="true" />
          Waiting for it. The board opens here by itself.
        </p>
      </div>
    </main>
  );
}
