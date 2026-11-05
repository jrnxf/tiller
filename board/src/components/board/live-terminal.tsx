import type { WebglAddon } from "@xterm/addon-webgl";
import type { ITerminalInitOnlyOptions, ITerminalOptions, Terminal } from "@xterm/xterm";
import "@xterm/xterm/css/xterm.css";
import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  useSyncExternalStore,
} from "react";
import type { ReactNode } from "react";
import { createPortal } from "react-dom";

import { createStore, useStore } from "../../lib/store";
import { CloseIcon } from "../shell/icons";
import { formatLisbonMoment } from "./format";
import {
  ArrowIcon,
  ChevronLeftIcon,
  EnterIcon,
  ExpandIcon,
  GlobeIcon,
  KeyboardIcon,
  LockIcon,
  PointerIcon,
  RetryIcon,
  TerminalIcon,
  WarnIcon,
} from "./icons";
import { typedKeys } from "./terminal-keys";
import { TerminalInput, TerminalStream } from "./terminal-stream";
import type { MirrorStatus, TerminalSize } from "./terminal-stream";
import { showToast } from "./toast";
import "./terminal.css";

// Whether running attempts show their terminal. Per browser, like the board's
// look: it is a way of looking, and closed until the owner opens one.
const OPEN_KEY = "tiller-terminal-open";

function loadOpen(): boolean {
  try {
    return localStorage.getItem(OPEN_KEY) === "1";
  } catch {
    return false;
  }
}

const terminalOpenStore = createStore<boolean>(loadOpen());

function toggleTerminal() {
  terminalOpenStore.update((open) => {
    try {
      localStorage.setItem(OPEN_KEY, open ? "0" : "1");
    } catch {
      // Blocked storage: the choice lasts for this tab.
    }
    return !open;
  });
}

export function useTerminalOpen(): boolean {
  return useStore(terminalOpenStore);
}

export function TerminalToggle({ size }: { size?: "sm" }) {
  const open = useTerminalOpen();
  return (
    <button
      type="button"
      className={`btn mir-tog${size ? ` ${size}` : ""}`}
      aria-pressed={open}
      title={open ? "Hide the terminal" : "Show the live terminal"}
      onClick={toggleTerminal}
    >
      <TerminalIcon />
      Terminal
    </button>
  );
}

const PHONE = "(max-width: 820px)";

function subscribePhone(change: () => void) {
  const media = window.matchMedia?.(PHONE);
  media?.addEventListener?.("change", change);
  return () => media?.removeEventListener?.("change", change);
}

function usePhone(): boolean {
  return useSyncExternalStore(
    subscribePhone,
    () => window.matchMedia?.(PHONE)?.matches === true,
    () => false,
  );
}

// The pane is a real terminal, so it stays dark in both themes, like herdr.
const SCREEN = "#0d0e10";
const THEME = {
  background: SCREEN,
  foreground: "#c9ced6",
  cursor: "#e6e9ee",
  cursorAccent: SCREEN,
  selectionBackground: "#5e6ad266",
  black: "#1d1f23",
  red: "#ef7c7c",
  green: "#5fcf86",
  yellow: "#f0bf00",
  blue: "#7fb6f5",
  magenta: "#b4a0ff",
  cyan: "#6fd1d8",
  white: "#c9ced6",
  brightBlack: "#6f7680",
  brightRed: "#f2a0a0",
  brightGreen: "#8be0a8",
  brightYellow: "#f2a46b",
  brightBlue: "#a5cbf8",
  brightMagenta: "#cdbfff",
  brightCyan: "#9fe2e7",
  brightWhite: "#e6e9ee",
};
const FONT = '"Geist Mono", ui-monospace, "SF Mono", Menlo, monospace';
const BASE_FONT = 12;
const OPTIONS: ITerminalOptions & ITerminalInitOnlyOptions = {
  cols: 80,
  rows: 24,
  fontSize: BASE_FONT,
  fontFamily: FONT,
  lineHeight: 1.25,
  scrollback: 0,
  theme: THEME,
  cursorBlink: true,
  cursorStyle: "block",
  cursorInactiveStyle: "block",
  // Until the stream is live: keys typed before then would go nowhere.
  disableStdin: true,
};
const MIN_FONT = 4;
const MAX_FONT = 20;
// The screen's inner padding on each side, in CSS pixels.
const PAD = 8;

type View = "inline" | "dialog" | "sheet";
type Scale = "fit" | "one";

// A glyph's width over the font size, for the terminal font.
let glyphRatio = 0;
function advance(): number {
  if (glyphRatio) return glyphRatio;
  try {
    const context = document.createElement("canvas").getContext("2d");
    if (context) {
      context.font = `100px ${FONT}`;
      glyphRatio = context.measureText("W").width / 100;
    }
  } catch {
    // No canvas: the usual monospace width below.
  }
  glyphRatio ||= 0.6;
  return glyphRatio;
}

function fontReady(): Promise<unknown> {
  try {
    return document.fonts.load(`${BASE_FONT}px "Geist Mono"`).catch(() => null);
  } catch {
    return Promise.resolve(null);
  }
}

function list(names: string[]): string {
  if (names.length < 2) return names.join("");
  return `${names.slice(0, -1).join(", ")} and ${names.at(-1)}`;
}

function StatusMark({ status }: { status: MirrorStatus }) {
  const [dot, label] =
    status.kind === "live"
      ? ["live", "Live"]
      : status.kind === "reconnecting"
        ? ["stale", "Reconnecting"]
        : status.kind === "connecting"
          ? ["stale", "Connecting"]
          : status.kind === "ended"
            ? ["idle", "Ended"]
            : status.kind === "unreachable"
              ? ["off", "herdr unreachable"]
              : ["off", "No terminal"];
  return (
    <span className="mir-st">
      <i className={`ld ${dot}`} />
      {label}
    </span>
  );
}

function Message({
  icon,
  title,
  children,
  onRetry,
}: {
  icon: ReactNode;
  title: string;
  children: ReactNode;
  onRetry?: () => void;
}) {
  return (
    <div className="mir-msg">
      {icon}
      <div>
        <b>{title}</b>
        {children}
      </div>
      {onRetry ? (
        <button type="button" className="btn sm" onClick={onRetry}>
          <RetryIcon />
          Retry
        </button>
      ) : null}
    </div>
  );
}

// Why a remote agent has no mirror. Shown in place of the terminal.
export function RemoteTerminalNote({ machine }: { machine: string }) {
  return (
    <div className="mir">
      <Message icon={<GlobeIcon />} title={`Runs on ${machine}`}>
        The board mirrors terminals only for agents on this machine. Open it in herdr instead.
      </Message>
    </div>
  );
}

// What the mirror shows in place of a screen, when it has none to show.
function NoScreen({ status, onRetry }: { status: MirrorStatus; onRetry: () => void }) {
  switch (status.kind) {
    case "forbidden":
      return (
        <Message icon={<LockIcon />} title="Only the owner sees the terminal">
          This browser is not signed in as the owner. Run <code>tiller board --login</code> for a
          login link.
        </Message>
      );
    case "remote":
      return (
        <Message icon={<GlobeIcon />} title="Runs on another machine">
          {status.detail}
        </Message>
      );
    case "unreachable":
      return (
        <Message icon={<WarnIcon />} title={status.title} onRetry={onRetry}>
          {status.detail}
        </Message>
      );
    case "failed":
      return (
        <Message icon={<WarnIcon />} title="The terminal is not available" onRetry={onRetry}>
          {status.detail}
        </Message>
      );
    default:
      return null;
  }
}

// Keys typed to the agent are not board shortcuts.
const contain = (event: KeyboardEvent) => event.stopPropagation();

// One terminal with its stream and its keys, wired to the host it draws in.
// The WebGL renderer draws box and block characters itself, so they meet
// cell for cell at any font size; the DOM renderer leans on whatever font has
// them. Without WebGL, or once its context is lost, xterm draws with the DOM.
function openRenderer(term: Terminal, Webgl: typeof WebglAddon | null) {
  if (!Webgl) return;
  try {
    const addon = new Webgl();
    addon.onContextLoss(() => addon.dispose());
    term.loadAddon(addon);
  } catch {
    // No WebGL here: the DOM renderer stays.
  }
}

function mountTerminal(
  term: Terminal,
  sessionId: string,
  host: HTMLElement,
  report: {
    status: (status: MirrorStatus) => void;
    size: (size: TerminalSize) => void;
    typing: (typing: boolean) => void;
  },
): { input: TerminalInput; stream: TerminalStream; dispose: () => void } {
  const input = new TerminalInput(sessionId, (title, detail) => showToast("bad", title, detail));
  const stream = new TerminalStream(
    sessionId,
    {
      reset(cols, rows) {
        term.reset();
        term.resize(cols, rows);
      },
      write(bytes) {
        term.write(bytes);
      },
    },
    report.status,
    report.size,
  );
  const data = term.onData((typed) => {
    const { keys, unsent } = typedKeys(typed);
    input.keys(keys);
    if (unsent.length > 0) {
      showToast(
        "bad",
        `herdr can't send ${list(unsent)}`,
        "Nothing was sent for that key. Use herdr itself for it.",
      );
    }
  });
  // A paste goes as one piece of text, not as typed keys.
  const onPaste = (event: ClipboardEvent) => {
    event.preventDefault();
    event.stopPropagation();
    const text = event.clipboardData?.getData("text/plain") ?? "";
    if (text && !term.options.disableStdin) input.text(text);
  };
  // The only thing in the host that takes focus is xterm's input.
  const focus = () => report.typing(true);
  const blur = () => report.typing(false);
  host.addEventListener("paste", onPaste, { capture: true });
  host.addEventListener("keydown", contain);
  host.addEventListener("keyup", contain);
  host.addEventListener("focusin", focus);
  host.addEventListener("focusout", blur);
  return {
    input,
    stream,
    dispose() {
      stream.stop();
      input.close();
      data.dispose();
      host.removeEventListener("paste", onPaste, { capture: true });
      host.removeEventListener("keydown", contain);
      host.removeEventListener("keyup", contain);
      host.removeEventListener("focusin", focus);
      host.removeEventListener("focusout", blur);
      term.dispose();
    },
  };
}

// The agent's herdr pane, live, at the pane's own size and scaled to fit. One
// xterm and one stream serve the inline view and the expanded one, so
// expanding keeps what is on screen. Clicking the screen types to the agent:
// keys go through herdr, and the board's own shortcuts pause meanwhile.
export function LiveTerminal({
  sessionId,
  label,
  heading,
  actions,
}: {
  sessionId: string;
  // What the terminal is, for assistive tech: "build on beta-7".
  label: string;
  // The expanded view's title row, and its actions (Open in herdr).
  heading: ReactNode;
  actions?: ReactNode;
}) {
  const phone = usePhone();
  const [host] = useState(() => {
    const element = document.createElement("div");
    element.className = "mir-x";
    return element;
  });
  const termRef = useRef<Terminal | null>(null);
  const inputRef = useRef<TerminalInput | null>(null);
  const streamRef = useRef<TerminalStream | null>(null);
  // Where the xterm sits in each view; state, so effects follow the element.
  const [inlineBox, setInlineBox] = useState<HTMLDivElement | null>(null);
  const [bigBox, setBigBox] = useState<HTMLDivElement | null>(null);
  const [status, setStatus] = useState<MirrorStatus>({ kind: "connecting" });
  const [size, setSize] = useState<TerminalSize | null>(null);
  const [typing, setTyping] = useState(false);
  const [view, setView] = useState<View>("inline");
  const [scale, setScale] = useState<Scale>("fit");
  const [font, setFont] = useState(BASE_FONT);
  // The web font is in, so xterm measures its cells right when it opens.
  const [ready, setReady] = useState(false);
  // Focus the screen once the sheet holds it: a phone tap means "type".
  const focusNext = useRef(false);
  const webglRef = useRef<typeof WebglAddon | null>(null);

  useEffect(() => {
    let gone = false;
    let unmount: (() => void) | null = null;
    // xterm loads with the first terminal the owner opens, not with the board.
    void Promise.all([import("@xterm/xterm"), import("@xterm/addon-webgl"), fontReady()]).then(
      ([xterm, webgl]) => {
        if (gone) return;
        const term = new xterm.Terminal(OPTIONS);
        webglRef.current = webgl.WebglAddon;
        const mounted = mountTerminal(term, sessionId, host, {
          status: setStatus,
          size: setSize,
          typing: setTyping,
        });
        termRef.current = term;
        inputRef.current = mounted.input;
        streamRef.current = mounted.stream;
        unmount = () => {
          mounted.dispose();
          termRef.current = null;
          inputRef.current = null;
          streamRef.current = null;
          webglRef.current = null;
        };
        setReady(true);
      },
      () => {
        if (gone) return;
        setStatus({ kind: "failed", detail: "The terminal did not load. Reload the page." });
      },
    );
    return () => {
      gone = true;
      unmount?.();
      setReady(false);
      setTyping(false);
    };
  }, [sessionId, host]);

  const live = status.kind === "live";
  useEffect(() => {
    const term = termRef.current;
    if (!term) return;
    term.options.disableStdin = !live;
    if (!live) term.blur();
  }, [live]);

  const box = view === "inline" ? inlineBox : bigBox;
  // The font size that shows the whole pane in the room the slot has, or
  // the base size for 1:1. The pane keeps its own size in herdr: only the
  // picture scales, and through the font, so text stays sharp. Cells are
  // whole device pixels, so the cell is picked first and the font is the one
  // whose glyphs fill it: a font between two cells would crowd its glyphs.
  const layout = useCallback(() => {
    const term = termRef.current;
    const screen = host.querySelector<HTMLElement>(".xterm-screen");
    if (!term || !box || !screen || !screen.offsetHeight) return;
    const current = term.options.fontSize ?? BASE_FONT;
    const fit = view === "inline" || scale === "fit";
    const grow = view === "dialog" && scale === "fit";
    const width = box.clientWidth - PAD * 2;
    const dpr = window.devicePixelRatio || 1;
    const ratio = advance() * dpr;
    // The largest font the room's height allows, from the rows' height now.
    const tallest = grow
      ? Math.max(200, window.innerHeight - 240) / (screen.offsetHeight / current)
      : Number.POSITIVE_INFINITY;
    const least = Math.ceil(MIN_FONT * ratio);
    let cell = Math.floor(Math.min(grow ? MAX_FONT : BASE_FONT, tallest) * ratio);
    if (fit) cell = Math.min(cell, Math.floor((width * dpr) / term.cols));
    cell = Math.max(least, cell);
    // A hair over the cell, so xterm's own rounding lands on it.
    const sized = (cells: number) => (cells + 0.05) / ratio;
    if (sized(cell) !== current) term.options.fontSize = sized(cell);
    // A measure off by a pixel can still come out wide: step down until it
    // is inside.
    if (fit) {
      for (
        let tries = 0;
        tries < 4 && screen.offsetWidth > width + 0.5 && cell > least;
        tries += 1
      ) {
        cell -= 1;
        term.options.fontSize = sized(cell);
      }
    }
    setFont(term.options.fontSize ?? BASE_FONT);
  }, [host, box, view, scale]);

  // Put the one xterm in the view that shows it, opening it there the first
  // time (xterm measures its cells on open, so the host must be on the page),
  // then size it.
  useLayoutEffect(() => {
    const term = termRef.current;
    if (!box || !ready || !term) return;
    if (host.parentElement !== box) box.appendChild(host);
    if (!term.element) {
      term.open(host);
      openRenderer(term, webglRef.current);
      term.textarea?.setAttribute("aria-label", `Type to ${label}`);
      streamRef.current?.start();
    }
    layout();
    if (focusNext.current) {
      focusNext.current = false;
      term.focus();
    }
  }, [box, ready, host, label, layout]);

  // The pane changes size, or the room for it does: fit it again.
  useEffect(() => {
    if (!box || typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(() => layout());
    observer.observe(box);
    observer.observe(host);
    return () => observer.disconnect();
  }, [box, host, layout]);

  // On a phone at 1:1, keep the cursor in view as the agent writes.
  useEffect(() => {
    const term = termRef.current;
    if (!term || !ready || !bigBox || view !== "sheet" || scale !== "one") return;
    const reveal = () => {
      // The sheet's body, which scrolls; the host sits in it.
      const pane = host.parentElement;
      const screen = host.querySelector<HTMLElement>(".xterm-screen");
      if (!pane || !screen) return;
      const cellWidth = screen.offsetWidth / term.cols;
      const cellHeight = screen.offsetHeight / term.rows;
      const x = PAD + term.buffer.active.cursorX * cellWidth;
      const y = PAD + term.buffer.active.cursorY * cellHeight;
      const outX = x < pane.scrollLeft || x > pane.scrollLeft + pane.clientWidth - cellWidth * 4;
      const outY = y < pane.scrollTop || y > pane.scrollTop + pane.clientHeight - cellHeight * 2;
      if (!outX && !outY) return;
      pane.scrollTo({
        left: outX ? Math.max(0, x - pane.clientWidth / 2) : pane.scrollLeft,
        top: outY ? Math.max(0, y - pane.clientHeight / 2) : pane.scrollTop,
      });
    };
    reveal();
    const moved = term.onCursorMove(reveal);
    return () => moved.dispose();
  }, [host, bigBox, ready, view, scale]);

  const retry = useCallback(() => streamRef.current?.retry(), []);
  const done = useCallback(() => termRef.current?.blur(), []);
  const send = useCallback((key: string) => inputRef.current?.keys([key]), []);

  const expand = useCallback(() => {
    if (phone) {
      setScale("one");
      setView("sheet");
      return;
    }
    // Fit grows the pane to fill the dialog, which is the point of opening it.
    setScale("fit");
    setView("dialog");
  }, [phone]);

  const collapse = useCallback(() => {
    termRef.current?.blur();
    setView("inline");
  }, []);

  const ended = status.kind === "ended";
  const screenShown =
    status.kind === "live" ||
    status.kind === "connecting" ||
    status.kind === "reconnecting" ||
    (ended && size !== null);
  const message = screenShown ? null : ended ? (
    <Message icon={<TerminalIcon />} title="The agent's pane is closed">
      herdr no longer has a pane for this agent.
    </Message>
  ) : (
    <NoScreen status={status} onRetry={retry} />
  );
  const pct = Math.round((font / BASE_FONT) * 100);
  const sizeText = size ? `${size.cols} × ${size.rows} · ${pct}%` : null;
  const state = `${typing ? " typing" : ""}${status.kind === "reconnecting" ? " dim" : ""}${ended ? " ended" : ""}`;
  const veil =
    status.kind === "reconnecting" || status.kind === "connecting" ? (
      <div className="mir-veil">
        <span className="mir-pill">
          <span className="mir-spin" />
          {status.kind === "connecting" ? "Connecting" : "Reconnecting"}
        </span>
      </div>
    ) : null;
  const endedAt = ended ? formatLisbonMoment(status.at.toISOString()) : null;
  const footer = (where: "inline" | "big") =>
    typing ? (
      <>
        <span className="tx">
          <KeyboardIcon />
          Typing goes to the agent. Board shortcuts are paused.
        </span>
        <button type="button" className="btn sm" onClick={done}>
          Done
        </button>
      </>
    ) : status.kind === "reconnecting" ? (
      <span className="tx">
        The stream dropped. Opening it again for a fresh full screen (try {status.attempt}).
      </span>
    ) : status.kind === "connecting" ? (
      <span className="tx">Waiting for the first screen from herdr.</span>
    ) : ended ? (
      <span className="tx">The pane closed {endedAt}. This is its last screen.</span>
    ) : where === "inline" && view !== "inline" ? (
      <span className="tx">The terminal is open in the expanded view.</span>
    ) : (
      <span className="tx">
        <PointerIcon />
        {phone ? "Tap" : "Click"} the screen to type to the agent
      </span>
    );

  return (
    <div className={`mir${state}`}>
      <div className="mir-h">
        <StatusMark status={status} />
        {typing && view === "inline" ? <span className="lab sm acc">Typing</span> : null}
        {screenShown && sizeText && view === "inline" ? (
          <span className="mir-sz">{sizeText}</span>
        ) : null}
        <span className="sp" />
        {screenShown && !ended && view === "inline" ? (
          <button
            type="button"
            className="ib sm"
            title="Expand"
            aria-label="Expand the terminal"
            onClick={expand}
          >
            <ExpandIcon />
          </button>
        ) : null}
      </div>
      {message}
      {screenShown && view === "inline" ? (
        <div className="mir-s" role="group" aria-label={`Live terminal for ${label}`}>
          <div className="mir-slot" ref={setInlineBox} />
          {veil}
          {phone && live ? (
            <button
              type="button"
              className="mir-tap"
              aria-label="Type to the agent"
              onClick={() => {
                focusNext.current = true;
                expand();
              }}
            />
          ) : null}
        </div>
      ) : null}
      {screenShown ? <div className="mir-f">{footer("inline")}</div> : null}
      {view === "dialog"
        ? createPortal(
            <Expanded
              onClose={collapse}
              heading={heading}
              actions={actions}
              scale={scale}
              onScale={setScale}
              label={label}
            >
              <div className={`mir big${state}`}>
                <div className="mir-h">
                  <StatusMark status={status} />
                  {typing ? <span className="lab sm acc">Typing</span> : null}
                  {sizeText ? <span className="mir-sz">{sizeText}</span> : null}
                </div>
                {message}
                <div
                  className={`mir-s ${scale}`}
                  role="group"
                  aria-label={`Live terminal for ${label}`}
                  hidden={!screenShown}
                >
                  <div className="mir-slot" ref={setBigBox} />
                  {veil}
                </div>
                <div className="mir-f">{footer("big")}</div>
              </div>
            </Expanded>,
            document.body,
          )
        : null}
      {view === "sheet"
        ? createPortal(
            <div
              className={`mir-sheet${state}`}
              role="dialog"
              aria-modal="true"
              aria-label={`Terminal for ${label}`}
            >
              <div className="mir-sheet-h">
                <button type="button" className="ib" aria-label="Back" onClick={collapse}>
                  <ChevronLeftIcon />
                </button>
                <span className="mir-sheet-t">{heading}</span>
                <span className="sp" />
                <ScaleSwitch scale={scale} onScale={setScale} />
              </div>
              {message}
              <div
                className={`mir-sheet-b ${scale}`}
                ref={setBigBox}
                role="group"
                aria-label={`Live terminal for ${label}`}
                hidden={!screenShown}
              />
              <div className="mir-sheet-f">
                {typing ? (
                  <>
                    <KeyboardIcon />
                    Typing goes to the agent
                    <button type="button" className="btn sm" onClick={done}>
                      Done
                    </button>
                  </>
                ) : (
                  footer("big")
                )}
              </div>
              <div className="mir-keys" role="group" aria-label="Keys a phone lacks">
                {(
                  [
                    ["esc", "Esc", "Esc"],
                    ["tab", "Tab", "Tab"],
                    ["ctrl+c", "Ctrl-C", "Ctrl-C"],
                    ["left", <ArrowIcon key="l" to="left" />, "Left"],
                    ["up", <ArrowIcon key="u" to="up" />, "Up"],
                    ["down", <ArrowIcon key="d" to="down" />, "Down"],
                    ["right", <ArrowIcon key="r" to="right" />, "Right"],
                    ["enter", <EnterIcon key="e" />, "Enter"],
                  ] as const
                ).map(([key, face, name]) => (
                  <button
                    key={key}
                    type="button"
                    className={`mir-key${key === "ctrl+c" || key === "enter" ? " w" : ""}`}
                    aria-label={name}
                    disabled={!live}
                    // Keep the screen focused, so the phone keyboard stays up.
                    onPointerDown={(event) => event.preventDefault()}
                    onClick={() => send(key)}
                  >
                    {face}
                  </button>
                ))}
              </div>
            </div>,
            document.body,
          )
        : null}
    </div>
  );
}

function ScaleSwitch({ scale, onScale }: { scale: Scale; onScale: (scale: Scale) => void }) {
  return (
    <div className="seg mir-seg" role="group" aria-label="Scale">
      <button type="button" aria-pressed={scale === "fit"} onClick={() => onScale("fit")}>
        Fit
      </button>
      <button type="button" aria-pressed={scale === "one"} onClick={() => onScale("one")}>
        1:1
      </button>
    </div>
  );
}

// The large view: the same live terminal in a dialog. The backdrop and Escape
// close it (Escape typed to the agent stays with the agent), and Tab stays on
// the dialog's own controls.
function Expanded({
  onClose,
  heading,
  actions,
  scale,
  onScale,
  label,
  children,
}: {
  onClose: () => void;
  heading: ReactNode;
  actions?: ReactNode;
  scale: Scale;
  onScale: (scale: Scale) => void;
  label: string;
  children: ReactNode;
}) {
  const dialog = useRef<HTMLDivElement>(null);
  const close = useRef<HTMLButtonElement>(null);
  useEffect(() => {
    close.current?.focus();
  }, []);
  return (
    <div className="dialog-backdrop mir-backdrop" onMouseDown={onClose}>
      <div
        ref={dialog}
        className="mir-dlg"
        role="dialog"
        aria-modal="true"
        aria-label={`Terminal for ${label}`}
        onMouseDown={(event) => event.stopPropagation()}
        onKeyDown={(event) => {
          if (event.key === "Escape") {
            event.preventDefault();
            event.stopPropagation();
            onClose();
          } else if (event.key === "Tab") {
            const stops = [
              ...(dialog.current?.querySelectorAll<HTMLElement>("button, a[href]") ?? []),
            ].filter((element) => !(element as HTMLButtonElement).disabled);
            const [first, last] = [stops[0], stops.at(-1)];
            if (event.shiftKey && document.activeElement === first) {
              event.preventDefault();
              last?.focus();
            } else if (!event.shiftKey && document.activeElement === last) {
              event.preventDefault();
              first?.focus();
            }
          }
        }}
      >
        <div className="mir-dlg-h">
          {heading}
          <span className="sp" />
          <ScaleSwitch scale={scale} onScale={onScale} />
          {actions}
          <button
            ref={close}
            type="button"
            className="ib"
            aria-label="Close the expanded terminal"
            onClick={onClose}
          >
            <CloseIcon />
          </button>
        </div>
        <div className="mir-dlg-b">{children}</div>
      </div>
    </div>
  );
}
