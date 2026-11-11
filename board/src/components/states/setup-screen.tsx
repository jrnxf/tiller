import { useEffect } from "react";
import type { ReactNode } from "react";

import { SNAPSHOT_PATH } from "../../lib/boot";
import { fetchJson } from "../../lib/queries";
import { Logo } from "../shell/logo";
import { useAppearance } from "../shell/shell";

// How often the setup screen asks whether `tiller init` has run.
export const SETUP_POLL_MS = 2_000;

// What a command in the daemon's hint does, by how it starts. The hint holds
// one command per line, so the screen numbers and explains however many lines
// it has. A line the screen does not know shows with its number alone.
function noteFor(line: string): ReactNode {
  if (line.startsWith("tiller owner set ")) {
    return (
      <>
        Hand this machine to Tiller. <code>tiller init</code> refuses while another system owns it.
      </>
    );
  }
  if (line.startsWith("tiller init ")) {
    return (
      <>
        Name this machine and say how many agents it may run at once. On a work machine, add{" "}
        <code>--boundary work</code> to this command.
      </>
    );
  }
  return null;
}

// The daemon runs, but this machine has no state.db yet: a fresh install
// whose service started before anyone set it up. The screen names the
// commands to run and opens the board by itself once the daemon serves it.
export function SetupScreen({ command, onReady }: { command: string; onReady: () => void }) {
  useAppearance();
  const lines = command.split("\n");
  const numbered = lines.length > 1;

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
          <Logo />
        </div>
        <h1>Set up Tiller on this machine</h1>
        <p>
          The daemon is running, but this machine has no Tiller state yet.{" "}
          {numbered
            ? "Run these in a terminal, one at a time, in this order:"
            : "Run this in a terminal:"}
        </p>
        <ol className="setup-steps">
          {lines.map((line, step) => {
            const note = noteFor(line);
            return (
              <li key={line}>
                {numbered || note ? (
                  <p className="step-note">
                    {numbered ? <span className="step-n">{step + 1}.</span> : null}
                    {note}
                  </p>
                ) : null}
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
            );
          })}
        </ol>
        <p className="wait" role="status">
          <i aria-hidden="true" />
          Waiting for it. The board opens here by itself.
        </p>
      </div>
    </main>
  );
}
