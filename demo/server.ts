// Throwaway board demo (T0.5). Not part of the product; deleted at cutover.
//
// One Bun script: polls the Firstmate fleet snapshot and herdr (locally and on
// every saved machine), and pushes changes to the page over SSE.
//
//   bun demo/server.ts            # http://127.0.0.1:4777
//
// Read-only, with one exception: jump-to-pane for local panes, which only
// focuses a pane herdr already reported.

import { join } from "node:path"

const PORT = Number(process.env.PORT ?? 4777)
const FLEET_EVERY_MS = Number(process.env.FLEET_EVERY_MS ?? 15_000)
const HERDR_EVERY_MS = Number(process.env.HERDR_EVERY_MS ?? 4_000)
const FM_HOME = process.env.FIRSTMATE_HOME ?? `${process.env.HOME}/firstmate`
const SNAPSHOT_CMD = process.env.FLEET_SNAPSHOT_CMD ?? `${FM_HOME}/bin/fm-fleet-snapshot.sh`
const HERDR_SESSION = "default"
const ALLOWED_ORIGINS = new Set(
  [`http://127.0.0.1:${PORT}`, `http://localhost:${PORT}`].concat(
    (process.env.DEMO_ALLOWED_ORIGINS ?? "").split(",").map((o) => o.trim()).filter(Boolean),
  ),
)

type Json = any

async function run(cmd: string[], timeoutMs: number): Promise<string> {
  const proc = Bun.spawn(cmd, { stdout: "pipe", stderr: "ignore" })
  const timer = setTimeout(() => proc.kill(), timeoutMs)
  try {
    const [out, code] = await Promise.all([new Response(proc.stdout).text(), proc.exited])
    if (code !== 0) throw new Error(`${cmd[0]} exited ${code}`)
    return out
  } finally {
    clearTimeout(timer)
  }
}

// ---- state ---------------------------------------------------------------

type Task = {
  id: string
  project: string
  title: string
  kind: string | null
  column: "needs" | "active" | "queued" | "done"
  since: string | null
  prUrl: string | null
  state: string | null
  detail: string | null
  freshness: string | null
  paneId: string | null
}

type Pane = {
  paneId: string
  workspaceId: string
  workspace: string
  agent: string | null
  status: string
  title: string
  cwd: string
}

type HostView = {
  name: string
  local: boolean
  ok: boolean
  error: string | null
  updatedAt: number
  panes: Pane[]
}

const state = {
  fleet: { ok: false, error: null as string | null, updatedAt: 0, generated: null as string | null, tasks: [] as Task[] },
  hosts: new Map<string, HostView>(),
}
let localPaneIds = new Set<string>()
let localWorkspaceByPane = new Map<string, string>()

// ---- fleet snapshot ------------------------------------------------------

function toColumn(rec: Json, live: Json | undefined): Task["column"] {
  if (rec.state === "done") return "done"
  if (rec.captain_actionable || live?.hints?.pending_decision || rec.hold_bucket === "live") return "needs"
  if (rec.state === "in_flight") return "active"
  return "queued"
}

// Herdr targets look like "<session>:<pane-id>" (the pane id has its own colon).
// Only a local herdr task in the session this server reads has a jumpable pane.
function localPaneId(t: Json | undefined): string | null {
  const target: string | undefined = t?.endpoint?.target
  if (!target || t.remote != null || t.backend !== "herdr") return null
  const i = target.indexOf(":")
  return i > 0 && target.slice(0, i) === HERDR_SESSION ? target.slice(i + 1) : null
}

async function pollFleet() {
  try {
    const snap: Json = JSON.parse(await run([SNAPSHOT_CMD, "--json"], 60_000))
    const live = new Map<string, Json>((snap.tasks ?? []).map((t: Json) => [t.id, t]))
    const tasks: Task[] = (snap.backlog?.records ?? []).map((rec: Json): Task => {
      const t = live.get(rec.id)
      return {
        id: rec.id,
        project: rec.repo ?? "other",
        title: rec.title ?? rec.id,
        kind: rec.kind ?? null,
        column: toColumn(rec, t),
        since: rec.since ?? null,
        prUrl: rec.pr_url ?? t?.pr?.url ?? null,
        state: t?.current_state?.state ?? null,
        detail: t?.current_state?.detail ?? null,
        freshness: t?.current_state?.freshness ?? null,
        paneId: localPaneId(t),
      }
    })
    state.fleet = { ok: true, error: null as any, updatedAt: Date.now(), generated: snap.generated ?? null, tasks }
  } catch (e) {
    state.fleet = { ...state.fleet, ok: false, error: String(e), updatedAt: Date.now() }
  }
  broadcast()
}

// ---- herdr ---------------------------------------------------------------

function toPanes(snapshot: Json): Pane[] {
  const ws = new Map<string, string>((snapshot.workspaces ?? []).map((w: Json) => [w.workspace_id, w.label ?? w.workspace_id]))
  const agents = new Map<string, Json>((snapshot.agents ?? []).map((a: Json) => [a.pane_id, a]))
  return (snapshot.panes ?? []).map((p: Json): Pane => {
    const a = agents.get(p.pane_id)
    return {
      paneId: p.pane_id,
      workspaceId: p.workspace_id,
      workspace: ws.get(p.workspace_id) ?? p.workspace_id,
      agent: a?.agent ?? p.agent ?? null,
      status: a?.agent_status ?? p.agent_status ?? "unknown",
      title: p.terminal_title_stripped ?? p.terminal_title ?? "",
      cwd: p.cwd ?? "",
    }
  })
}

async function snapshotHost(name: string, local: boolean) {
  const cmd = local ? ["herdr", "api", "snapshot", "--session", HERDR_SESSION] : ["herdr", "--machine", name, "api", "snapshot"]
  const key = local ? "local" : `remote:${name}`
  const prev = state.hosts.get(key)
  try {
    const out: Json = JSON.parse(await run(cmd, 10_000))
    const panes = toPanes(out.result?.snapshot ?? {})
    state.hosts.set(key, { name, local, ok: true, error: null, updatedAt: Date.now(), panes })
    if (local) {
      localPaneIds = new Set(panes.map((p) => p.paneId))
      localWorkspaceByPane = new Map(panes.map((p) => [p.paneId, p.workspaceId]))
    }
  } catch (e) {
    // Keep the last good panes so a blip does not blank the column; flag it stale.
    state.hosts.set(key, { name, local, ok: false, error: String(e), updatedAt: prev?.updatedAt ?? 0, panes: prev?.panes ?? [] })
  }
}

async function pollHerdr() {
  let machines: string[] | null = null
  try {
    const list: Json = JSON.parse(await run(["herdr", "machine", "list", "--json"], 10_000))
    const rows: Json[] = Array.isArray(list) ? list : (list.machines ?? list.result?.machines ?? [])
    machines = rows.filter((m) => m.enabled !== false).map((m) => m.label ?? m.id).filter(Boolean)
  } catch {
    // Listing failed: keep the known machines and mark them stale below.
  }
  if (machines === null) machines = [...state.hosts.values()].filter((h) => !h.local).map((h) => h.name)
  const wanted = new Set(machines.map((m) => `remote:${m}`).concat("local"))
  for (const k of [...state.hosts.keys()]) if (!wanted.has(k)) state.hosts.delete(k)
  await Promise.all([snapshotHost("local", true), ...machines.map((m) => snapshotHost(m, false))])
  broadcast()
}

// ---- SSE -----------------------------------------------------------------

// REDACT=1 swaps every project, task and pane name for a generic one, so the
// page can be screenshotted for a public PR without leaking private names.
const REDACT = process.env.REDACT === "1"

function redact(fleet: typeof state.fleet, hosts: HostView[]) {
  const names = new Map<string, string>()
  const alias = (p: string) => (names.get(p) ?? names.set(p, `project-${String.fromCharCode(97 + names.size)}`).get(p)!)
  const tasks = fleet.tasks.map((t, i) => ({
    ...t,
    project: alias(t.project),
    id: `${alias(t.project)}-${i + 1}`,
    title: `Example task ${i + 1}`,
    detail: null,
    prUrl: null,
    paneId: null,
  }))
  const panes = (h: HostView) => h.panes.map((p, i) => ({ ...p, workspace: `workspace-${p.workspaceId}`, title: `agent session ${i + 1}`, cwd: "/home/user/project" }))
  return {
    fleet: { ...fleet, tasks, error: fleet.error ? "unavailable" : null },
    hosts: hosts.map((h, i) => ({ ...h, name: h.local ? h.name : `machine-${i}`, panes: panes(h), error: h.error ? "unavailable" : null })),
  }
}

function payload() {
  const hosts = [...state.hosts.values()]
  return { now: Date.now(), ...(REDACT ? redact(state.fleet, hosts) : { fleet: state.fleet, hosts }) }
}

const clients = new Set<ReadableStreamDefaultController<Uint8Array>>()
const enc = new TextEncoder()
let lastSent = ""

function broadcast() {
  const body = payload()
  // Compare without volatile timestamps so idle polls push nothing.
  const key = JSON.stringify({ f: { ...body.fleet, updatedAt: 0 }, h: body.hosts.map((h) => ({ ...h, updatedAt: 0 })) })
  if (key === lastSent) return
  lastSent = key
  const frame = enc.encode(`event: state\ndata: ${JSON.stringify(body)}\n\n`)
  for (const c of clients) {
    try {
      c.enqueue(frame)
    } catch {
      clients.delete(c)
    }
  }
}

// ---- jump to pane --------------------------------------------------------

async function focusPane(paneId: string): Promise<{ ok: boolean; error?: string; scope?: "pane" | "workspace" }> {
  // Only panes herdr itself reported on the local host; never pass arbitrary input through.
  if (!localPaneIds.has(paneId)) return { ok: false, error: "unknown local pane" }
  try {
    await run(["herdr", "agent", "focus", paneId, "--session", HERDR_SESSION], 5_000)
    return { ok: true, scope: "pane" }
  } catch {
    const ws = localWorkspaceByPane.get(paneId)
    if (!ws) return { ok: false, error: "focus failed" }
    try {
      await run(["herdr", "workspace", "focus", ws, "--session", HERDR_SESSION], 5_000)
    } catch (e) {
      return { ok: false, error: String(e) }
    }
    return { ok: true, scope: "workspace" }
  }
}

// ---- http ----------------------------------------------------------------

const page = Bun.file(join(import.meta.dir, "index.html"))

const server = Bun.serve({
  hostname: "127.0.0.1",
  port: PORT,
  idleTimeout: 0,
  async fetch(req) {
    const url = new URL(req.url)
    if (url.pathname === "/") return new Response(page, { headers: { "content-type": "text/html; charset=utf-8" } })
    if (url.pathname === "/api/state") return Response.json(payload())
    if (url.pathname === "/events") {
      let ctl!: ReadableStreamDefaultController<Uint8Array>
      const stream = new ReadableStream<Uint8Array>({
        start(c) {
          ctl = c
          clients.add(c)
          c.enqueue(enc.encode(`event: state\ndata: ${JSON.stringify(payload())}\n\n`))
        },
        cancel() {
          clients.delete(ctl)
        },
      })
      return new Response(stream, {
        headers: { "content-type": "text/event-stream", "cache-control": "no-cache", connection: "keep-alive" },
      })
    }
    if (url.pathname === "/api/focus" && req.method === "POST") {
      // Cross-site pages must not be able to drive this: require same-origin.
      const origin = req.headers.get("origin")
      if (origin && !ALLOWED_ORIGINS.has(origin)) {
        return Response.json({ ok: false, error: `origin ${origin} not allowed` }, { status: 403 })
      }
      const body: Json = await req.json().catch(() => ({}))
      const res = await focusPane(String(body.paneId ?? ""))
      return Response.json(res, { status: res.ok ? 200 : 400 })
    }
    return new Response("not found", { status: 404 })
  },
})

// Keep idle connections alive through proxies.
setInterval(() => {
  const ping = enc.encode(`: ping\n\n`)
  for (const c of clients) {
    try {
      c.enqueue(ping)
    } catch {
      clients.delete(c)
    }
  }
}, 20_000)

// Overlap-free loops: the next poll is scheduled after the previous one ends.
async function loop(fn: () => Promise<void>, every: number) {
  for (;;) {
    await fn().catch(() => {})
    await Bun.sleep(every)
  }
}
void loop(pollFleet, FLEET_EVERY_MS)
void loop(pollHerdr, HERDR_EVERY_MS)

console.log(`board demo on http://127.0.0.1:${server.port}`)
