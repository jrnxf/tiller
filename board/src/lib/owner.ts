// The owner token: the half of the owner session this board keeps
// (protocol.md section 7.1). Signing in sets the `tiller_owner` cookie and
// hands the board a token, which it keeps in localStorage and sends on every
// request as `X-Tiller-Owner`. A cookie goes to every port of its host, so a
// server on another loopback port could be handed it; localStorage belongs
// to this origin alone, so that server never has the token, and the cookie
// alone reads nothing.

export const OWNER_HEADER = "x-tiller-owner";
const STORAGE_KEY = "tiller-owner-token";
// The login link's redirect puts the token in the URL fragment, which a
// browser never sends to a server.
const FRAGMENT = "owner=";

export function ownerToken(): string | null {
  try {
    return window.localStorage.getItem(STORAGE_KEY);
  } catch {
    return null;
  }
}

export function keepOwnerToken(token: string): void {
  try {
    window.localStorage.setItem(STORAGE_KEY, token);
  } catch {
    // Storage is blocked: every read answers signed out, and the board says so.
  }
}

export function dropOwnerToken(): void {
  try {
    window.localStorage.removeItem(STORAGE_KEY);
  } catch {
    // Nothing kept, nothing to drop.
  }
}

// Keeps the token a login link's redirect put in the fragment, and takes it
// out of the address bar and the history entry. Returns whether it found one.
export function takeOwnerTokenFromFragment(win: Window = window): boolean {
  const { hash, pathname, search } = win.location;
  if (!hash.startsWith(`#${FRAGMENT}`)) return false;
  const token = hash.slice(FRAGMENT.length + 1);
  win.history.replaceState(win.history.state, "", `${pathname}${search}`);
  if (!/^[0-9a-f]+$/.test(token)) return false;
  keepOwnerToken(token);
  return true;
}

// `init` with the owner token added to its headers, when this board has one.
function withOwner(init: RequestInit = {}): RequestInit {
  const token = ownerToken();
  if (token === null) return init;
  const headers = new Headers(init.headers);
  headers.set(OWNER_HEADER, token);
  return { ...init, headers };
}

// `fetch`, with the owner token: what every request to the daemon goes through.
export function ownerFetch(path: string, init?: RequestInit): Promise<Response> {
  return fetch(path, withOwner(init));
}
