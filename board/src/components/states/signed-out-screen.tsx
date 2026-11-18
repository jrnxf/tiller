import { REGEXP_ONLY_DIGITS } from "input-otp";
import { useEffect, useRef, useState } from "react";
import type { ReactNode } from "react";

import { CODE_LENGTH, codeDigits, signIn } from "../../lib/session";
import { Logo } from "../shell/logo";
import { InputOTP, InputOTPGroup, InputOTPSeparator, InputOTPSlot } from "../ui/input-otp";
import { useAppearance } from "../shell/shell";

// `text` with each `command` span set as code.
function withCode(text: string): ReactNode[] {
  return text
    .split("`")
    .map((part, index) =>
      index % 2 === 1 ? <code key={index}>{part}</code> : <span key={index}>{part}</span>,
    );
}

// A browser without the owner cookie sees only this, on loopback as at an
// `--allow-host` name: eight boxes for the code `tiller board --login` prints. It sends
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
        <h1>Enter your sign-in code</h1>
        <form
          className="signin-form"
          onSubmit={(event) => {
            event.preventDefault();
            submit(code);
          }}
        >
          <InputOTP
            ref={field}
            maxLength={CODE_LENGTH}
            pattern={REGEXP_ONLY_DIGITS}
            pasteTransformer={codeDigits}
            aria-label="Sign-in code"
            aria-invalid={error ? true : undefined}
            aria-describedby={error ? "signin-error" : undefined}
            autoComplete="one-time-code"
            value={code}
            readOnly={pending}
            onChange={(value) => {
              const digits = codeDigits(value);
              setCode(digits);
              if (digits.length > 0) setError(null);
              submit(digits);
            }}
          >
            <InputOTPGroup>
              {[0, 1, 2, 3].map((index) => (
                <InputOTPSlot key={index} index={index} />
              ))}
            </InputOTPGroup>
            <InputOTPSeparator />
            <InputOTPGroup>
              {[4, 5, 6, 7].map((index) => (
                <InputOTPSlot key={index} index={index} />
              ))}
            </InputOTPGroup>
          </InputOTP>
        </form>
        {error ? (
          <p className="signin-error" id="signin-error" role="alert">
            {withCode(error)}
          </p>
        ) : null}
        <p className="sub signin-hint">
          Get a code with <code>tiller board --login</code> on the machine running tiller.
        </p>
      </div>
    </main>
  );
}
