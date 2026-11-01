# Board demo

Throwaway demo of the Tiller board. It is not part of the product and is deleted at cutover.

```
bun demo/server.ts      # http://127.0.0.1:4777
```

- Polls the Firstmate fleet snapshot every 15 s and herdr every 4 s, locally and on each saved machine.
- Pushes changes to the page over SSE (`/events`). Idle polls push nothing.
- Read-only. The one action is "Jump to pane", which focuses a pane herdr itself reported on this machine.
- Binds to loopback only.

Environment: `PORT`, `FIRSTMATE_HOME`, `FLEET_SNAPSHOT_CMD`, `FLEET_EVERY_MS`, `HERDR_EVERY_MS`, `DEMO_ALLOWED_ORIGINS`. Local herdr is always read and focused in its `default` session.

"Jump to pane" accepts requests only from `http://127.0.0.1:PORT` and `http://localhost:PORT`. When the board is viewed through a proxy, add its origin, comma separated:

```
DEMO_ALLOWED_ORIGINS=https://board.example.com bun demo/server.ts
```

Open `/?theme=light` or `/?theme=dark` to force a theme; otherwise it follows the system.
