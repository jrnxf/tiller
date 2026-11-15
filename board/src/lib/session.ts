import { ReadError } from "./queries";
import { createStore } from "./store";
import { dropOwnerToken, keepOwnerToken, ownerFetch } from "./owner";

// Signing in with the code `tiller board --login` prints, and signing out
// (protocol.md section 7.1). The code and the link are one login: either
// works once, within a minute. Signing in sets the owner cookie and hands
// over the owner token, which this board keeps (owner.ts); every read needs
// both.

export const CODE_LENGTH = 8;
const LOGIN_PATH = "/api/v1/login";
const LOGOUT_PATH = "/api/v1/logout";

// True once this browser has signed out, or a read found its cookie gone: the
// app shows the signed-out screen in place of the board.
export const signedOutStore = createStore(false);

// A read refused because this browser is not signed in: every read needs
// the owner cookie and its owner token, on loopback as at an `--allow-host`
// name.
export function isSignedOut(error: unknown): boolean {
  return error instanceof ReadError && error.status === 403 && error.code === "signed_out";
}

// Only digits, at most eight: what typing or pasting "1234 5678" leaves.
export function codeDigits(text: string): string {
  return text.replace(/\D/g, "").slice(0, CODE_LENGTH);
}

const RUN_AGAIN = "Run `tiller board --login` again for a new code.";

const REFUSALS: Record<string, string> = {
  wrong_code: `That code is wrong or has expired. ${RUN_AGAIN}`,
  code_burned: `Too many wrong codes, so that code no longer works. ${RUN_AGAIN}`,
  too_many_tries:
    "Too many codes were tried from this network in the last minute. Wait a minute and try again.",
  usage: "A code is 8 digits.",
};

const UNREACHABLE = "The daemon did not answer. Check the connection and try again.";

export type SignIn = { ok: true } | { ok: false; message: string };

// The outcome, and the refusal's code when the daemon gave one.
async function post(
  path: string,
  body: Record<string, unknown>,
): Promise<{ result: SignIn; code: string | null; body: unknown }> {
  let response: Response;
  try {
    response = await ownerFetch(path, {
      method: "POST",
      headers: { "content-type": "application/json", accept: "application/json" },
      body: JSON.stringify(body),
    });
  } catch {
    return { result: { ok: false, message: UNREACHABLE }, code: null, body: null };
  }
  if (response.ok) {
    const answer: unknown = await response.json().catch(() => null);
    return { result: { ok: true }, code: null, body: answer };
  }
  let code: unknown = null;
  try {
    code = ((await response.json()) as { error?: { code?: unknown } }).error?.code;
  } catch {
    // Reported by status below.
  }
  const known = typeof code === "string" ? REFUSALS[code] : undefined;
  return {
    result: { ok: false, message: known ?? `The daemon answered ${response.status}. Try again.` },
    code: typeof code === "string" ? code : null,
    body: null,
  };
}

// Sends the code; on success the response has set the owner cookie and
// names the owner token, which is kept here.
export async function signIn(code: string): Promise<SignIn> {
  const { result, body } = await post(LOGIN_PATH, { code });
  if (!result.ok) return result;
  const token = (body as { owner_token?: unknown } | null)?.owner_token;
  if (typeof token !== "string") {
    return { ok: false, message: "The daemon signed in but sent no owner token. Try again." };
  }
  keepOwnerToken(token);
  return result;
}

// Ends this browser's owner session on the daemon and drops the cookie and
// the owner token here. Every read now needs a session this browser no
// longer has, so the app shows the signed-out screen. A session already gone
// counts as signed out.
export async function signOut(): Promise<SignIn> {
  const { result, code } = await post(LOGOUT_PATH, {});
  if (code === "signed_out" || result.ok) {
    dropOwnerToken();
    signedOutStore.set(true);
    return { ok: true };
  }
  return result;
}
