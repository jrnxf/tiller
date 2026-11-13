import type { ActionError } from "./api";

// A 504 `forward_uncertain` is not a refusal: the peer may have applied the
// change (protocol.md 10.3). The control keeps its idempotency key, so pressing
// it again is safe: the peer answers from what it stored, or applies it once.
// Interrupt, stop, resume, relaunch and an answer wait 60 s for the peer's
// proof, a project add, retire or delete 11 min for its clone and stops; every
// other forwarded call waits 10 s (protocol.md 7.4, 10.3).
function isSentUncertain(error: ActionError | null): boolean {
  return error?.status === 504 && error.code === "forward_uncertain";
}

export function ActionFailure({
  error,
  host,
  proof = false,
  project = false,
}: {
  error: ActionError | null | undefined;
  host: string | undefined;
  proof?: boolean;
  project?: boolean;
}) {
  if (!error) return null;
  if (isSentUncertain(error)) {
    return (
      <p className="sent-unsure" role="alert" data-testid="sent-not-confirmed">
        <b>Sent, not confirmed.</b> {host ?? "The other host"} did not answer within{" "}
        {project ? "11 minutes" : proof ? "60 seconds" : "10 seconds"}, so this may or may not have
        been applied. Press the button again to retry: it carries the same key, so it is applied
        once.
      </p>
    );
  }
  return (
    <p className="refusal" role="alert">
      {error.message}
    </p>
  );
}
