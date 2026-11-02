import { createStore } from "./store";

// `connecting` is the state before the first open; the degraded pill shows only
// for `reconnecting`, so a normal load never shows it.
export type ConnectionState = "connecting" | "live" | "reconnecting";

export const connectionStore = createStore<ConnectionState>("connecting");
