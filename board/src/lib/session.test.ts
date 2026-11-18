import { afterEach, beforeEach, expect, test, vi } from "vitest";

import { ReadError, agentsQuery, boardQuery, healthQuery } from "./queries";
import { sendMutation } from "../components/attention/api";
import { ownerToken } from "./owner";
import {
  codeDigits,
  handoff,
  handoffLink,
  isSignedOut,
  signIn,
  signOut,
  signedOutStore,
} from "./session";
import { createTestQueryClient } from "../test-app";

beforeEach(() => {
  signedOutStore.set(false);
  window.localStorage.clear();
});
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

test("the code goes to the login endpoint as JSON, and the owner token is kept", async () => {
  const fetch = answer(200, { owner: true, owner_token: "f1c7" });
  expect(await signIn("12345678")).toEqual({ ok: true });
  expect(fetch).toHaveBeenCalledWith("/api/v1/login", {
    method: "POST",
    headers: { "content-type": "application/json", accept: "application/json" },
    body: '{"code":"12345678"}',
  });
  expect(ownerToken()).toBe("f1c7");
});

test("a sign-in answer without an owner token is no sign-in", async () => {
  answer(200, { owner: true });
  expect((await signIn("12345678")).ok).toBe(false);
  expect(ownerToken()).toBeNull();
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
  expect(await signIn("12345678")).toMatchObject({ message: /from this network.*Wait a minute/ });
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

  // Every read needs the cookie, on loopback too (G24), so no read is needed
  // to know the board is now signed out.
  window.localStorage.setItem("tiller-owner-token", "f1c7");
  const fetch = signOutAnswers([200, { owner: false }], [200, { owner: false }]);
  expect(await signOut()).toEqual({ ok: true });
  expect(ownerToken()).toBeNull();
  expect(fetch).toHaveBeenCalledWith("/api/v1/logout", expect.objectContaining({ method: "POST" }));
  expect(fetch).toHaveBeenCalledTimes(1);
  expect(signedOutStore.get()).toBe(true);
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
  const queryClient = createTestQueryClient();
  await queryClient.fetchQuery(boardQuery()).catch(() => {});
  expect(signedOutStore.get()).toBe(false);

  answer(403, refusal("signed_out"));
  await queryClient.fetchQuery({ ...agentsQuery(), retry: false }).catch(() => {});
  expect(signedOutStore.get()).toBe(true);
});

test("health with only ok, version and initialized means the cookie is gone", async () => {
  const queryClient = createTestQueryClient();
  answer(200, { ok: true, version: "0.1.0", initialized: true });
  await expect(queryClient.fetchQuery({ ...healthQuery(), retry: false })).rejects.toThrow(
    ReadError,
  );
  expect(signedOutStore.get()).toBe(true);

  signedOutStore.set(false);
  answer(200, { ok: true, version: "0.1.0", initialized: true, pollers: [] });
  await queryClient.fetchQuery({ ...healthQuery(), staleTime: 0, retry: false });
  expect(signedOutStore.get()).toBe(false);
});

const livePage = {
  host: "7440.board.example.com",
  origin: "https://7440.board.example.com",
  pathname: "/inbox",
  search: "?item=01ABC",
};

test("a live board signs in from the main board, back to the page it is on", () => {
  expect(handoffLink(livePage, "https://board.example.com")).toBe(
    "https://board.example.com/handoff?to=7440.board.example.com&path=%2Finbox%3Fitem%3D01ABC",
  );
});

test("no handoff link without a main board, with a bad one, or on the main board itself", () => {
  expect(handoffLink(livePage, "")).toBeNull();
  expect(handoffLink(livePage, "not a url")).toBeNull();
  expect(handoffLink(livePage, "https://7440.board.example.com/")).toBeNull();
  // The board the daemon serves is built with no main board.
  expect(handoffLink(livePage)).toBeNull();
});

test("a handoff asks for that name and path, and answers the login link", async () => {
  const fetch = answer(200, { url: "https://7440.board.example.com/login/ab12" });
  expect(await handoff("7440.board.example.com", "/inbox")).toEqual({
    ok: true,
    url: "https://7440.board.example.com/login/ab12",
  });
  const [path, init] = fetch.mock.calls[0] as unknown as [string, RequestInit];
  expect(path).toBe("/api/v1/login/handoff");
  expect(JSON.parse(String(init.body))).toEqual({ host: "7440.board.example.com", path: "/inbox" });
});

test("a handoff to a name the daemon does not allow says so", async () => {
  answer(400, refusal("usage"));
  expect(await handoff("evil.example.com", "/")).toEqual({
    ok: false,
    message: "This daemon does not allow evil.example.com, so it cannot sign it in.",
  });
});
