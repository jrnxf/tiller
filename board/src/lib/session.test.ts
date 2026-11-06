import { afterEach, beforeEach, expect, test, vi } from "vitest";

import { ReadError, agentsQuery, boardQuery } from "./queries";
import { createQueryClient } from "./query-client";
import { sendMutation } from "../components/attention/api";
import { codeDigits, isSignedOut, signIn, signOut, signedOutStore } from "./session";

beforeEach(() => signedOutStore.set(false));
afterEach(() => vi.unstubAllGlobals());

function answer(status: number, body: unknown) {
  const fetch = vi.fn(async () => new Response(JSON.stringify(body), { status }));
  vi.stubGlobal("fetch", fetch);
  return fetch;
}

function refusal(code: string) {
  return { ok: false, error: { code, message: "refused" } };
}

test("typing or pasting keeps only the first eight digits", () => {
  expect(codeDigits("1234 5678")).toBe("12345678");
  expect(codeDigits("12-34-56-78-90")).toBe("12345678");
  expect(codeDigits("abc")).toBe("");
});

test("only a 403 signed_out read means this browser is signed out", () => {
  expect(isSignedOut(new ReadError("/api/v1/board", 403, null, null, "signed_out"))).toBe(true);
  expect(isSignedOut(new ReadError("/api/v1/board", 403, null, null, "not_owner"))).toBe(false);
  expect(isSignedOut(new ReadError("/api/v1/board", 401, null, null, "signed_out"))).toBe(false);
  expect(isSignedOut(new Error("signed_out"))).toBe(false);
});

test("the code goes to the login endpoint as JSON", async () => {
  const fetch = answer(200, { owner: true });
  expect(await signIn("12345678")).toEqual({ ok: true });
  expect(fetch).toHaveBeenCalledWith("/api/v1/login", {
    method: "POST",
    headers: { "content-type": "application/json", accept: "application/json" },
    body: '{"code":"12345678"}',
  });
});

test("each refusal reads as plain text", async () => {
  answer(403, refusal("wrong_code"));
  expect(await signIn("12345678")).toEqual({
    ok: false,
    message: "That code is wrong or has expired. Run `tiller board --login` again for a new code.",
  });
  answer(403, refusal("code_burned"));
  expect(await signIn("12345678")).toMatchObject({ message: /^Too many wrong codes/ });
  answer(429, refusal("too_many_tries"));
  expect(await signIn("12345678")).toMatchObject({ message: /Wait a minute/ });
  answer(500, "not json either");
  expect(await signIn("12345678")).toEqual({
    ok: false,
    message: "The daemon answered 500. Try again.",
  });
  vi.stubGlobal("fetch", () => Promise.reject(new TypeError("offline")));
  expect(await signIn("12345678")).toMatchObject({ message: /did not answer/ });
});

// The logout answer, then what a read answers once it is done.
function signOutAnswers(logout: [number, unknown], read: [number, unknown]) {
  const fetch = vi.fn(async (input: RequestInfo | URL) => {
    const [status, body] = String(input) === "/api/v1/logout" ? logout : read;
    return new Response(JSON.stringify(body), { status });
  });
  vi.stubGlobal("fetch", fetch);
  return fetch;
}

test("signing out shows the signed-out screen only once the daemon has ended the cookie", async () => {
  signOutAnswers([403, refusal("not_owner")], [200, { owner: true }]);
  expect((await signOut()).ok).toBe(false);
  expect(signedOutStore.get()).toBe(false);

  const fetch = signOutAnswers([200, { owner: false }], [403, refusal("signed_out")]);
  expect(await signOut()).toEqual({ ok: true });
  expect(fetch).toHaveBeenCalledWith("/api/v1/logout", expect.objectContaining({ method: "POST" }));
  expect(fetch).toHaveBeenCalledWith("/api/v1/session", expect.anything());
  expect(signedOutStore.get()).toBe(true);
});

test("on loopback, where reads need no cookie, signing out keeps the board", async () => {
  signOutAnswers([200, { owner: false }], [200, { owner: false }]);
  expect(await signOut()).toEqual({ ok: true });
  expect(signedOutStore.get()).toBe(false);
});

test("a cookie already gone signs out at once", async () => {
  signOutAnswers([403, refusal("signed_out")], [403, refusal("signed_out")]);
  expect(await signOut()).toEqual({ ok: true });
  expect(signedOutStore.get()).toBe(true);
});

test("a mutation refused as signed out flips the board to the signed-out screen", async () => {
  answer(403, refusal("not_owner"));
  await sendMutation("POST", "/api/v1/cards", { idempotency_key: "k" }).catch(() => {});
  expect(signedOutStore.get()).toBe(false);

  answer(403, refusal("signed_out"));
  await expect(sendMutation("POST", "/api/v1/cards", { idempotency_key: "k" })).rejects.toThrow(
    "refused",
  );
  expect(signedOutStore.get()).toBe(true);
});

test("any read refused as signed out flips the board to the signed-out screen", async () => {
  answer(403, refusal("not_owner"));
  const queryClient = createQueryClient();
  await queryClient.fetchQuery(boardQuery()).catch(() => {});
  expect(signedOutStore.get()).toBe(false);

  answer(403, refusal("signed_out"));
  await queryClient.fetchQuery({ ...agentsQuery(), retry: false }).catch(() => {});
  expect(signedOutStore.get()).toBe(true);
});
