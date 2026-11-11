// Both ends of this host's links for the dev and preview servers and for
// tests (`GET /api/v1/peers`, protocol.md section 7.3), kept in memory so
// Unpair and Revoke key change what the next read answers.
//
// A federated page has host-a paired with host-d, host-e, host-f and host-g,
// and host-a accepted two boards: host-d's, live, so host-d's card can
// revoke it, and host-c's, revoked last week. host-f's key file sits in a
// folder this host cannot write, so the first Unpair of host-f leaves it, as
// the daemon reports one, and a second Unpair deletes it.
import type { AcceptedKey, Machine, PairedPeer, PeerKey, PeerList } from "./api-types.ts";
import type { Scenario } from "./fixtures.ts";

const PAIRED: readonly PairedPeer[] = [
  {
    label: "host-d",
    peer_host_id: "01JPEERD0000000000000000D",
    scope: "act",
    fingerprint: "SHA256:Qm4xT2vK8pLw3nR6yHc9ZbF1sJd5aE7uGq0oNi2Br9Tk",
  },
  {
    label: "host-e",
    peer_host_id: "01JPEERE0000000000000000E",
    scope: "act",
    fingerprint: "SHA256:Hn7cW1qZ4xVb8mL2kP6tY9rD3sF5gJ0aU1eO7iRw2Mx",
  },
  {
    label: "host-f",
    peer_host_id: "01JPEERF0000000000000000F",
    scope: "read",
    fingerprint: "SHA256:Tz2bN6mQ9wE4rY1uI8oP3aS7dF0gH5jK2lZ6xC9vB4n",
  },
  {
    label: "host-g",
    peer_host_id: "01JPEERG0000000000000000G",
    scope: "act",
    fingerprint: null,
  },
];

const ACCEPTED: readonly AcceptedKey[] = [
  {
    id: "01JKEYC00000000000000000C1",
    viewer_label: "host-c",
    viewer_host_id: "01JHOSTC00000000000000000C",
    scope: "read",
    fingerprint: "SHA256:Lp2cR8wQe5tY7uI1oP4aS9dF3gH6jK0lZ2xC5vB8nM1",
    accepted_at: "2026-10-29T18:12:00.000Z",
    revoked_at: "2026-11-07T12:03:00.000Z",
    rev: 2,
  },
  {
    id: "01JKEYD00000000000000000D1",
    viewer_label: "host-d",
    viewer_host_id: "01JPEERD0000000000000000D",
    scope: "act",
    fingerprint: "SHA256:Vb8nM1Lp2cR8wQe5tY7uI1oP4aS9dF3gH6jK0lZ2xC5",
    accepted_at: "2026-11-05T21:40:00.000Z",
    revoked_at: null,
    rev: 1,
  },
];

const withPeers = (scenario: Scenario) => scenario === "federated" || scenario === "stale-sessions";

// The public key text each accepted board signs with; a list row shows its
// fingerprint instead.
const PUBLIC_KEYS: Record<string, string> = {
  "01JKEYC00000000000000000C1":
    "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIHc2Lp2cR8wQe5tY7uI1oP4aS9dF3gH6jK0lZ2xC5vB8 tiller-host-c",
  "01JKEYD00000000000000000D1":
    "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIFd8Vb8nM1Lp2cR8wQe5tY7uI1oP4aS9dF3gH6jK0lZ2 tiller-host-d",
};

// The host whose key file an Unpair cannot delete the first time.
const STUCK = "host-f";

// The labels unpaired, the key files they left and the keys revoked in this
// page's life.
const unpaired = new Set<string>();
const left = new Set<string>();
const revoked = new Map<string, string>();

// A new page life, for tests.
export function forgetPeerChanges() {
  unpaired.clear();
  left.clear();
  revoked.clear();
}

function acceptedNow(): AcceptedKey[] {
  return ACCEPTED.map((key) => {
    const at = revoked.get(key.id);
    return at ? { ...key, revoked_at: at, rev: key.rev + 1 } : key;
  });
}

export function peersFor(scenario: Scenario): PeerList {
  if (!withPeers(scenario)) return { paired: [], accepted: [], key_files_left: [] };
  const keyFilesLeft = [...left];
  keyFilesLeft.sort();
  return {
    paired: PAIRED.filter((peer) => !unpaired.has(peer.label)),
    accepted: acceptedNow(),
    key_files_left: keyFilesLeft,
  };
}

// The key's row as the daemon answers a revoke: its public key, no
// fingerprint.
function keyRow(key: AcceptedKey): PeerKey {
  const { fingerprint: _fingerprint, ...row } = key;
  return { ...row, public_key: PUBLIC_KEYS[key.id] ?? "" };
}

// An unpaired host stays a row of its own, no longer a peer, as the daemon
// keeps it.
export function withoutUnpaired(machine: Machine): Machine {
  if (!unpaired.has(machine.label) || !machine.peer_scope) return machine;
  const { peer_scope: _scope, link: _link, link_reason: _reason, ...rest } = machine;
  return { ...rest, running_agents: null, rev: machine.rev + 1 };
}

export function anyUnpaired(): boolean {
  return unpaired.size > 0;
}

type PeerReply =
  | { ok: true; kind: "unpair"; label: string; changed: boolean; warning: string | null }
  | { ok: true; data: PeerKey; kind: "revoke" }
  | { ok: false; status: number; code: string; message: string };

// `DELETE /api/v1/peers/{label}` or `DELETE /api/v1/peer-keys/{id}`, or
// `null` when `path` is neither. Both are natural: a second call answers the
// row as it is. An unpair reports only whether a pairing ended, since the
// caller knows the host rows: a label that is no row is 404 there. An
// Unpair whose key file stays warns, as the daemon does.
export function peerLinkMutation(
  method: string,
  path: string,
  scenario: Scenario,
): PeerReply | null {
  if (method !== "DELETE") return null;
  const label = /^\/api\/v1\/peers\/([^/]+)$/.exec(path)?.[1];
  if (label !== undefined) {
    const changed =
      withPeers(scenario) && !unpaired.has(label) && PAIRED.some((peer) => peer.label === label);
    if (changed) unpaired.add(label);
    let warning: string | null = null;
    if (changed && label === STUCK) {
      left.add(label);
      warning = `${label} is unpaired, but its key file was not deleted (Permission denied (os error 13)); Unpair it again from its Settings > Hosts card, or run tiller peer remove ${label}`;
    } else if (!changed) left.delete(label);
    return { ok: true, kind: "unpair", label, changed, warning };
  }
  const id = /^\/api\/v1\/peer-keys\/([^/]+)$/.exec(path)?.[1];
  if (id === undefined) return null;
  const key = withPeers(scenario) ? ACCEPTED.find((row) => row.id === id) : undefined;
  if (!key) return { ok: false, status: 404, code: "not_found", message: `no peer key ${id}` };
  if (!key.revoked_at && !revoked.has(id)) revoked.set(id, new Date().toISOString());
  const now = acceptedNow().find((row) => row.id === id) ?? key;
  return { ok: true, data: keyRow(now), kind: "revoke" };
}
