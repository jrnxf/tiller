import { useEffect } from "react";
import type { ReactNode } from "react";

import { SNAPSHOT_PATH } from "../../lib/boot";
import { fetchJson } from "../../lib/queries";
import { Logo } from "../shell/logo";
import { useAppearance } from "../shell/shell";

// How often the setup screen asks whether `tiller init` has run.
export const SETUP_POLL_MS = 2_000;

// Which setup step a command in the daemon's hint is. The hint holds one
// command per line, so the screen numbers and explains however many lines it
// has. A line the screen does not know shows with its number alone.
export function stepOf(line: string): "owner" | "init" | null {
  if (line.includes("tiller owner set ")) return "owner";
  if (/\btiller init\b/.test(line)) return "init";
  return null;
}

const BOUNDARY: ReactNode = (
  <>
    On a work machine, add <code>--boundary work</code> to the <code>tiller init</code> command.
  </>
);

const NOTES: Record<"owner" | "init", ReactNode> = {
  owner: (
    <>
      Hand this machine to Tiller. <code>tiller init</code> refuses while another system owns it.
    </>
  ),
  init: (
    <>
      Name this machine and say how many agents it may run at once. On a work machine, add{" "}
      <code>--boundary work</code> to this command.
    </>
  ),
};

// The daemon runs, but this machine has no state.db yet: a fresh install
// whose service started before anyone set it up. The screen names the
// commands to run and opens the board by itself once the daemon serves it.
export function SetupScreen({ command, onReady }: { command: string; onReady: () => void }) {
  useAppearance();
  const lines = command.split("\n");
  const numbered = lines.length > 1;
  const steps = lines.map(stepOf);

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
        {/* role="list": Safari drops the list role when list-style is none. */}
        <ol className="setup-steps" role="list">
          {lines.map((line, step) => {
            const kind = steps[step];
            const note = kind ? NOTES[kind] : null;
            return (
              // The lines never reorder, and two of them may be the same.
              <li key={step}>
                {numbered || note ? (
                  <p className="step-note">
                    {numbered ? (
                      <span className="step-n" aria-hidden="true">
                        {step + 1}.
                      </span>
                    ) : null}
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
        {/* A hint whose init line the screen does not know still says where
            --boundary work goes. */}
        {steps.includes("init") ? null : <p className="sub">{BOUNDARY}</p>}
        <p className="wait" role="status">
          <i aria-hidden="true" />
          Waiting for it. The board opens here by itself.
        </p>
      </div>
    </main>
  );
}
