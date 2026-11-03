import type { ActionError } from "./api";

// A 504 `forward_uncertain` is not a refusal: the peer may have applied the
// change (protocol.md 10.3). The control keeps its idempotency key, so pressing
// it again is safe: the peer answers from what it stored, or applies it once.
function isSentUncertain(error: ActionError | null): boolean {
  return error?.status === 504 && error.code === "forward_uncertain";
}

export function ActionFailure({
  error,
  host,
}: {
  error: ActionError | null | undefined;
  host: string | undefined;
}) {
  if (!error) return null;
  if (isSentUncertain(error)) {
    return (
      <p className="sent-unsure" role="alert" data-testid="sent-not-confirmed">
        <b>Sent, not confirmed.</b> {host ?? "The other host"} did not answer within 10 seconds, so
        this may or may not have been applied. Press the button again to retry: it carries the same
        key, so it is applied once.
      </p>
    );
  }
  return (
    <p className="refusal" role="alert">
      {error.message}
    </p>
  );
}
