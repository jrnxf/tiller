import { useState } from "react";

import { handoff } from "../../lib/session";

// The signed-in board's question before it signs another name in: that name
// gets a session of its own, so the owner says yes to each one, here, with
// the name in plain sight. A yes sends this browser there through a
// one-time login link.
export function HandoffPage({ to, path }: { to?: string; path?: string }) {
  const [error, setError] = useState<string | null>(null);
  const [pending, setPending] = useState(false);

  if (to === undefined) {
    return (
      <main className="setup" data-testid="handoff">
        <div className="setup-card signin-card">
          <h1>This link names no board</h1>
          <p className="sub">Open the board you want, and use its sign-in button.</p>
        </div>
      </main>
    );
  }

  function go(host: string) {
    setPending(true);
    setError(null);
    void handoff(host, path ?? "/").then((result) => {
      if (result.ok) {
        window.location.assign(result.url);
        return;
      }
      setError(result.message);
      setPending(false);
    });
  }

  return (
    <main className="setup" data-testid="handoff">
      <div className="setup-card signin-card">
        <h1>
          Sign in on <span className="handoff-host">{to}</span>?
        </h1>
        <p className="sub">It opens signed in as you. Say yes only to a board you started.</p>
        <div className="handoff-actions">
          <a href="/" className="btn">
            Cancel
          </a>
          <button
            type="button"
            className="btn primary"
            // The one question on the page: Enter answers it.
            autoFocus
            disabled={pending}
            onClick={() => go(to)}
          >
            Sign in there
          </button>
        </div>
        {error ? (
          <p className="signin-error" role="alert">
            {error}
          </p>
        ) : null}
      </div>
    </main>
  );
}
