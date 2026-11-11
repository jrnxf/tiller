import { useEffect, useRef, useState } from "react";
import type { ReactNode } from "react";

import { CODE_LENGTH, codeDigits, signIn } from "../../lib/session";
import { Logo } from "../shell/logo";
import { useAppearance } from "../shell/shell";

// `text` with each `command` span set as code.
function withCode(text: string): ReactNode[] {
  return text
    .split("`")
    .map((part, index) =>
      index % 2 === 1 ? <code key={index}>{part}</code> : <span key={index}>{part}</span>,
    );
}

// At an `--allow-host` name, a browser without the owner cookie sees only
// this: one field for the 8-digit code `tiller board --login` prints. It sends
// the code as the eighth digit lands, typed or pasted, and opens the board once
// the daemon has set the cookie. The link and QR code that command prints sign
// in the same way.
export function SignedOutScreen({ onSignedIn }: { onSignedIn: () => void }) {
  useAppearance();
  const [code, setCode] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [pending, setPending] = useState(false);
  const field = useRef<HTMLInputElement>(null);

  useEffect(() => {
    if (!pending) field.current?.focus();
  }, [pending]);

  function submit(digits: string) {
    if (pending || digits.length !== CODE_LENGTH) return;
    setPending(true);
    setError(null);
    void signIn(digits).then((result) => {
      if (result.ok) {
        onSignedIn();
        return;
      }
      setCode("");
      setError(result.message);
      setPending(false);
    });
  }

  return (
    <main className="setup" data-testid="signed-out">
      <div className="setup-card signin-card">
        <div className="ws">
          <Logo />
        </div>
        <h1>Sign in to this board</h1>
        <p>
          Run <code>tiller board --login</code> in a terminal on the machine running Tiller, then
          enter the 8-digit code it prints.
        </p>
        <form
          className="signin-form"
          onSubmit={(event) => {
            event.preventDefault();
            submit(code);
          }}
        >
          <input
            ref={field}
            className="signin-code"
            aria-label="Sign-in code"
            aria-invalid={error ? true : undefined}
            aria-describedby={error ? "signin-error" : undefined}
            inputMode="numeric"
            autoComplete="one-time-code"
            pattern="[0-9]*"
            spellCheck={false}
            placeholder="········"
            value={code}
            readOnly={pending}
            onChange={(event) => {
              const digits = codeDigits(event.target.value);
              setCode(digits);
              if (digits.length > 0) setError(null);
              submit(digits);
            }}
          />
        </form>
        {error ? (
          <p className="signin-error" id="signin-error" role="alert">
            {withCode(error)}
          </p>
        ) : null}
        <p className="sub signin-alt">
          It also prints a link and a QR code that sign in the same way. Each works once, within a
          minute.
        </p>
      </div>
    </main>
  );
}
