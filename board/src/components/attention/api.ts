import type { ApiErrorBody, MutationResponse } from "../../lib/api-types";
import { signedOutStore } from "../../lib/session";

// A refused or failed mutation, with the server's readable message, the
// `current` facts a refusal carries and its hint (protocol.md sections 6.1
// and 7.2).
export class ActionError extends Error {
  readonly status: number;
  readonly code: string;
  readonly current: Record<string, unknown> | null;
  readonly hint: string | null;
  // The peer that answered or refused a forwarded mutation; null for this host.
  readonly host: string | null;

  constructor(
    status: number,
    code: string,
    message: string,
    current?: Record<string, unknown>,
    hint?: string,
    host?: string,
  ) {
    super(message);
    this.name = "ActionError";
    this.status = status;
    this.code = code;
    this.current = current ?? null;
    this.hint = hint ?? null;
    this.host = host ?? null;
  }
}

const UNREACHABLE =
  "The daemon did not answer, so this may not be saved yet. Trying again is safe: the same click is never recorded twice.";

function isErrorBody(value: unknown): value is ApiErrorBody {
  if (typeof value !== "object" || value === null) return false;
  const error = (value as { error?: unknown }).error;
  return typeof error === "object" && error !== null && "message" in error;
}

// Sends one board mutation. Every body carries the control's idempotency key,
// so a retry of the same click replays the first answer instead of acting twice.
export async function sendMutation<T>(
  method: "POST" | "PATCH" | "PUT" | "DELETE",
  path: string,
  body: Record<string, unknown> & { idempotency_key: string },
): Promise<MutationResponse<T>> {
  let response: Response;
  try {
    response = await fetch(path, {
      method,
      headers: { "content-type": "application/json", accept: "application/json" },
      body: JSON.stringify(body),
    });
  } catch {
    throw new ActionError(0, "unavailable", UNREACHABLE);
  }
  let parsed: unknown = null;
  try {
    parsed = await response.json();
  } catch {
    // A body that is not JSON is reported by status below.
  }
  if (response.ok && parsed && (parsed as { ok?: unknown }).ok === true) {
    return parsed as MutationResponse<T>;
  }
  if (isErrorBody(parsed)) {
    const { code, message, current, hint, host } = parsed.error;
    // At an `--allow-host` name the cookie was revoked or ran out.
    if (response.status === 403 && code === "signed_out") signedOutStore.set(true);
    throw new ActionError(response.status, code, message, current, hint, host);
  }
  if (response.status === 503 || response.status === 0) {
    throw new ActionError(response.status, "unavailable", UNREACHABLE);
  }
  throw new ActionError(
    response.status,
    "internal",
    `The daemon answered ${response.status}, so this was not saved.`,
  );
}
