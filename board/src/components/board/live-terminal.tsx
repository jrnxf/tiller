import type { WebglAddon } from "@xterm/addon-webgl";
import type { ITerminalInitOnlyOptions, ITerminalOptions, Terminal } from "@xterm/xterm";
import "@xterm/xterm/css/xterm.css";
import {
  useCallback,
  useEffect,
  useImperativeHandle,
  useLayoutEffect,
  useRef,
  useState,
  useSyncExternalStore,
} from "react";
import type { MouseEvent as ReactMouseEvent, ReactNode, Ref } from "react";

import { canFullScreen, usePageFullScreen } from "../../lib/fullscreen";
import { shortcutsBlocked } from "../../lib/shortcuts";
import { createStore, useStore } from "../../lib/store";
import { CloseIcon } from "../shell/icons";
import { Modal } from "../ui/modal";
import { Ago, useTick } from "./ago";
import { formatLisbonMoment, secondsSince } from "./format";
import {
  ArrowIcon,
  ChevronLeftIcon,
  EnterIcon,
  ExpandIcon,
  FitIcon,
  FontLargerIcon,
  FontSmallerIcon,
  FullScreenIcon,
  GlobeIcon,
  HistoryIcon,
  KeyboardIcon,
  LeaveFullScreenIcon,
  LockIcon,
  PointerIcon,
  RetryIcon,
  TerminalIcon,
  WarnIcon,
} from "./icons";
import { pressTypes, typedKeys } from "./terminal-keys";
import { TerminalInput, TerminalStream, TerminalTail, readHistory } from "./terminal-stream";
import type { Fitting, Mirror, MirrorStatus, Screen, TerminalSize } from "./terminal-stream";
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

// Whether the Agents view's stage fits the pane to itself, one choice per
// kind of pane. A herdr pane is off until the owner turns it on, since the Fit
// changes the pane for herdr's own window too. A pane in the terminal holder
// has no other window, so it fits until the owner turns it off.
export const FIT_KEY = "tiller-terminal-fit";
export const HOLDER_FIT_KEY = "tiller-terminal-fit-holder";

function fitChoice(key: string, unset: boolean) {
  const load = () => {
    try {
      const stored = localStorage.getItem(key);
      return stored === null ? unset : stored === "1";
    } catch {
      return unset;
    }
  };
  const store = createStore<boolean>(load());
  const toggle = () =>
    store.update((fit) => {
      try {
        localStorage.setItem(key, fit ? "0" : "1");
      } catch {
        // Blocked storage: the choice lasts for this tab.
      }
      return !fit;
    });
  return { store, toggle, load };
}

const herdrFit = fitChoice(FIT_KEY, false);
const holderFit = fitChoice(HOLDER_FIT_KEY, true);

// A pane id the terminal holder gave: `p` and a ULID (terminal-holder.md 2.4).
// Any other pane id is herdr's (`w2:p1`). The session's pane id alone says
// which of the two runs its pane, as `tiller_core::ids::is_holder_pane` does.
const HOLDER_PANE = /^p[0-7][0-9A-HJKMNP-TV-Z]{25}$/i;

export function isHolderPane(pane: string | null | undefined): boolean {
  return typeof pane === "string" && HOLDER_PANE.test(pane);
}

// The stage's font size, per browser: the size the Fit sizes the pane at, and
// the largest the stage scales a pane it does not fit to.
export const FONT_KEY = "tiller-terminal-font";
export const STAGE_FONT_MIN = 9;
export const STAGE_FONT_MAX = 24;
// The stage grows a small pane into its room, but not past a size that reads
// as a terminal rather than a poster.
export const STAGE_FONT = 14;

export function clampStageFont(size: number): number {
  if (!Number.isFinite(size)) return STAGE_FONT;
  return Math.min(STAGE_FONT_MAX, Math.max(STAGE_FONT_MIN, Math.round(size)));
}

function loadStageFont(): number {
  try {
    const stored = localStorage.getItem(FONT_KEY);
    // Nothing stored, or nothing that reads as a number, is the default.
    return stored === null || stored.trim() === "" ? STAGE_FONT : clampStageFont(Number(stored));
  } catch {
    return STAGE_FONT;
  }
}

const stageFontStore = createStore<number>(loadStageFont());

// Reads the stage's choices from storage again, as a new tab would: for tests.
export function reloadTerminalPrefs() {
  herdrFit.store.set(herdrFit.load());
  holderFit.store.set(holderFit.load());
  stageFontStore.set(loadStageFont());
}

function setStageFont(size: number) {
  const next = clampStageFont(size);
  stageFontStore.set(next);
  try {
    if (next === STAGE_FONT) localStorage.removeItem(FONT_KEY);
    else localStorage.setItem(FONT_KEY, String(next));
  } catch {
    // Blocked storage: the size lasts for this tab.
  }
}

// A press on a view control leaves focus where it was, so typing goes on.
const keepFocus = (event: ReactMouseEvent) => event.preventDefault();

function subscribeVisible(change: () => void) {
  document.addEventListener("visibilitychange", change);
  return () => document.removeEventListener("visibilitychange", change);
}

// Whether the board is on screen: herdr has its pane back while it is not.
function useVisible(): boolean {
  return useSyncExternalStore(
    subscribeVisible,
    () => document.visibilityState !== "hidden",
    () => true,
  );
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

export function usePhone(): boolean {
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
  selectionBackground: "#b2c9a166",
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
// Seconds after which a tail's age reads as old: two reads missed.
const TAIL_OLD = 15;
// The rows of scrollback a history read asks for: herdr's own cap.
const HISTORY_ROWS = 1000;
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

// The cell xterm draws at a font size, in device pixels, worked out the way
// its renderers do: the glyph's width floored, its height ceiled, then the
// line height floored. Null where the canvas cannot say.
type Cell = { width: number; height: number };
const measured = new Map<string, Cell>();
function cellAt(fontSize: number, dpr: number): Cell | null {
  const key = `${fontSize}@${dpr}`;
  const known = measured.get(key);
  if (known) return known;
  const cell = measureCell(fontSize, dpr);
  if (cell) measured.set(key, cell);
  return cell;
}

function measureCell(fontSize: number, dpr: number): Cell | null {
  try {
    const context = document.createElement("canvas").getContext("2d");
    if (!context) return null;
    context.font = `${fontSize}px ${FONT}`;
    const metrics = context.measureText("W");
    const high = metrics.fontBoundingBoxAscent + metrics.fontBoundingBoxDescent;
    if (!metrics.width || !high) return null;
    return {
      width: Math.floor(metrics.width * dpr),
      height: Math.floor(Math.ceil(high * dpr) * (OPTIONS.lineHeight ?? 1)),
    };
  } catch {
    return null;
  }
}

// The daemon's bounds on a Fit (protocol.md 7.4).
const clampFit = (cols: number, rows: number): TerminalSize => ({
  cols: Math.min(500, Math.max(20, cols)),
  rows: Math.min(200, Math.max(5, rows)),
});

// How long the stage's size has to hold before the pane follows it.
const REFIT_DELAY = 300;

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
  useTick();
  // A tail's dot goes stale with its age, as its footer does.
  const fresh = status.kind === "tail" && (secondsSince(status.at) ?? Infinity) <= TAIL_OLD;
  const [dot, label] =
    status.kind === "live"
      ? ["live", "Live"]
      : status.kind === "tail"
        ? [fresh ? "live" : "stale", "Read only"]
        : status.kind === "reconnecting"
          ? ["stale", "Reconnecting"]
          : status.kind === "connecting"
            ? ["stale", "Connecting"]
            : status.kind === "ended"
              ? ["idle", "Ended"]
              : status.kind === "unreachable"
                ? ["off", status.mark ?? "herdr unreachable"]
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

// The keys that leave one agent's screen for another while typing: Option-J
// and Option-K step through the rail, Option-N jumps to the next agent that
// needs the owner. By key position, since Option changes the character.
export type SwitchKey = "next" | "prev" | "needs";
const SWITCH_KEYS: Record<string, SwitchKey> = { KeyJ: "next", KeyK: "prev", KeyN: "needs" };

function switchKey(event: KeyboardEvent): SwitchKey | null {
  if (event.type !== "keydown" || !event.altKey || event.ctrlKey || event.metaKey) return null;
  if (event.shiftKey) return null;
  return SWITCH_KEYS[event.code] ?? null;
}

// What the Agents view asks of its terminal: start typing to the agent.
export interface TerminalControl {
  type(): void;
}

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

// A tail's screen never holds focus.
const refuseFocus = (event: FocusEvent) => (event.target as HTMLElement).blur();

function mountTerminal(
  term: Terminal,
  sessionId: string,
  host: HTMLElement,
  report: {
    status: (status: MirrorStatus) => void;
    size: (size: TerminalSize) => void;
    typing: (typing: boolean) => void;
  },
  fitting: Fitting,
  tail: boolean,
  peer: string | undefined,
): { input: TerminalInput | null; stream: Mirror; dispose: () => void } {
  const screen: Screen = {
    reset(cols, rows) {
      term.reset();
      term.resize(cols, rows);
    },
    write(bytes) {
      term.write(bytes);
    },
  };
  // A tail is read only: no keys, no pastes, and nothing to type into. A
  // click never focuses xterm's input, so the board's keys keep working.
  if (tail) {
    const stream = new TerminalTail(sessionId, screen, report.status, report.size);
    host.addEventListener("focusin", refuseFocus);
    return {
      input: null,
      stream,
      dispose() {
        stream.stop();
        host.removeEventListener("focusin", refuseFocus);
        term.dispose();
      },
    };
  }
  // A peer's pane is read only too: the board host relays its stream, and
  // there is no path back into the pane (protocol.md 10.7).
  if (peer !== undefined) {
    const stream = new TerminalStream(sessionId, screen, report.status, report.size, null, peer);
    host.addEventListener("focusin", refuseFocus);
    return {
      input: null,
      stream,
      dispose() {
        stream.stop();
        host.removeEventListener("focusin", refuseFocus);
        term.dispose();
      },
    };
  }
  const input = new TerminalInput(sessionId, (title, detail) => showToast("bad", title, detail));
  const stream = new TerminalStream(sessionId, screen, report.status, report.size, fitting);
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
  variant,
  control,
  autoType = false,
  onTyping,
  onSwitch,
  machine,
  peer,
  holder = false,
  strip,
  onClose,
}: {
  sessionId: string;
  // What the terminal is, for assistive tech: "build on beta-7".
  label: string;
  // The expanded view's title row, and its actions (Open in herdr).
  heading: ReactNode;
  actions?: ReactNode;
  // The Agents view's stage: the screen fills the room it is given, with no
  // header row, and the footer carries the size and the expand button. A
  // popup is the expanded view alone, open from the start, over the page it
  // was opened from: closing it calls `onClose`.
  variant?: "stage" | "popup";
  control?: Ref<TerminalControl>;
  // Start typing as soon as the pane is live: the owner switched agents from
  // the keyboard while typing. Read once, when the terminal mounts.
  autoType?: boolean;
  onTyping?: (typing: boolean) => void;
  onSwitch?: (key: SwitchKey) => void;
  // The saved machine the agent runs on. herdr streams only its own panes,
  // so this one is read as a tail every few seconds, read only.
  machine?: string;
  // The paired peer host the agent runs on. Its board host relays the
  // stream, read only: no keys, no Fit.
  peer?: string;
  // The pane is in the terminal holder, not herdr: the stage fits it by
  // default, since no other window shows it.
  holder?: boolean;
  // What the stage shows between the screen and its footer: the answer buttons.
  strip?: ReactNode;
  onClose?: () => void;
}) {
  const phone = usePhone();
  const tail = machine !== undefined;
  // A tail or a peer's pane: nothing here types into it.
  const watch = tail || peer !== undefined;
  const stage = variant === "stage";
  const popup = variant === "popup";
  const [host] = useState(() => {
    const element = document.createElement("div");
    element.className = "mir-x";
    return element;
  });
  const termRef = useRef<Terminal | null>(null);
  const inputRef = useRef<TerminalInput | null>(null);
  const streamRef = useRef<Mirror | null>(null);
  // Where the xterm sits in each view; state, so effects follow the element.
  const [inlineBox, setInlineBox] = useState<HTMLDivElement | null>(null);
  const [bigBox, setBigBox] = useState<HTMLDivElement | null>(null);
  const [status, setStatus] = useState<MirrorStatus>({ kind: "connecting" });
  const [size, setSize] = useState<TerminalSize | null>(null);
  const [typing, setTyping] = useState(false);
  // A popup opens as the large view: the sheet at 1:1 on a phone, the dialog
  // fitted on a computer.
  const [view, setView] = useState<View>(popup ? (phone ? "sheet" : "dialog") : "inline");
  const [scale, setScale] = useState<Scale>(popup && phone ? "one" : "fit");
  const [font, setFont] = useState(BASE_FONT);
  // The Fit is the stage's on a computer, while the board is on screen, the
  // stage is not expanded, and herdr has not refused it for this agent.
  const fitChoiceFor = holder ? holderFit : herdrFit;
  const fitWanted = useStore(fitChoiceFor.store);
  const stageFont = useStore(stageFontStore);
  // The stage in full screen: its own element, so a dialog or another
  // agent's stage never reads as it.
  const full = usePageFullScreen() && stage;
  // Whether the stage font sets the pane's size now. Without the Fit, the
  // stage's room can set a smaller one, and a larger font changes nothing.
  const [capped, setCapped] = useState(true);
  const visible = useVisible();
  const [unfit, setUnfit] = useState(false);
  const fitting = stage && !phone && fitWanted && !unfit && visible && view === "inline";
  const fittingRef = useRef(fitting);
  // The stage's room in cells at the stage's font, kept up by the layout.
  const fitTarget = useRef<TerminalSize | null>(null);
  const stripBox = useRef<HTMLDivElement>(null);
  const refitTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  // The web font is in, so xterm measures its cells right when it opens.
  const [ready, setReady] = useState(false);
  // The pane's scrollback, read on the owner's ask and shown over the live
  // screen until they go back to it.
  const [history, setHistory] = useState<{ text: string } | "reading" | null>(null);
  const historyAsk = useRef<AbortController | null>(null);
  const closeHistory = useCallback(() => {
    historyAsk.current?.abort();
    historyAsk.current = null;
    setHistory(null);
  }, []);
  // Focus the screen once the sheet holds it: a phone tap means "type".
  const focusNext = useRef(false);
  // The sheet opened to type: the screen takes focus, not the sheet's Back.
  const [sheetTypes, setSheetTypes] = useState(false);
  // Type once the pane is live: asked for before the stream had a screen.
  const typeWhenLive = useRef(autoType);
  const webglRef = useRef<typeof WebglAddon | null>(null);
  const switchRef = useRef(onSwitch);
  useEffect(() => {
    switchRef.current = onSwitch;
  }, [onSwitch]);

  useEffect(() => {
    let gone = false;
    let unmount: (() => void) | null = null;
    // xterm loads with the first terminal the owner opens, not with the board.
    void Promise.all([import("@xterm/xterm"), import("@xterm/addon-webgl"), fontReady()]).then(
      ([xterm, webgl]) => {
        if (gone) return;
        const term = new xterm.Terminal(OPTIONS);
        term.attachCustomKeyEventHandler((event) => {
          const key = switchKey(event);
          if (!key || !switchRef.current) return true;
          event.preventDefault();
          switchRef.current(key);
          return false;
        });
        webglRef.current = webgl.WebglAddon;
        const mounted = mountTerminal(
          term,
          sessionId,
          host,
          {
            status: (next) => {
              setStatus(next);
              // A closed pane has no scrollback left to read.
              if (next.kind === "ended") closeHistory();
            },
            size: setSize,
            typing: (on) => {
              setTyping(on);
              // Typing goes to the live screen, so the scrollback gives way.
              if (on) closeHistory();
            },
          },
          {
            size: () => (fittingRef.current ? fitTarget.current : null),
            onUnfit: (detail) => {
              setUnfit(true);
              showToast("bad", "The pane kept its own size", detail);
            },
          },
          tail,
          peer,
        );
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
      // Another agent's pane starts from nothing, not from this one's state.
      setStatus({ kind: "connecting" });
      setSize(null);
      setUnfit(false);
      closeHistory();
    };
  }, [sessionId, host, closeHistory, tail, peer]);

  const live = status.kind === "live";
  useEffect(() => {
    const term = termRef.current;
    if (!term) return;
    term.options.disableStdin = !live || peer !== undefined;
    if (!live) term.blur();
    else if (typeWhenLive.current && term.element) {
      typeWhenLive.current = false;
      term.focus();
    }
  }, [live, peer]);

  useEffect(() => {
    onTyping?.(typing);
  }, [typing, onTyping]);
  // A terminal that goes away mid-typing never sees its focus leave.
  useEffect(() => () => onTyping?.(false), [onTyping]);

  // The Fit turned on or off: the stream opens again with it, or without.
  useEffect(() => {
    fittingRef.current = fitting;
    streamRef.current?.refit();
  }, [fitting]);
  useEffect(
    () => () => {
      if (refitTimer.current !== null) clearTimeout(refitTimer.current);
    },
    [],
  );

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
    // The height the rows may take: the dialog's window, or the stage's own
    // box.
    const room = grow
      ? Math.max(200, window.innerHeight - 240)
      : stage && view === "inline"
        ? box.clientHeight - PAD * 2
        : Number.POSITIVE_INFINITY;
    const least = Math.ceil(MIN_FONT * ratio);
    const most = grow ? MAX_FONT : stage && view === "inline" ? stageFont : BASE_FONT;
    // A hair over the cell, so xterm's own rounding lands on it.
    const sized = (cells: number) => (cells + 0.05) / ratio;
    // What the stage holds at its own font, for the Fit. A new size waits
    // for the stage to settle, so a drag of the window opens the stream once.
    if (stage && view === "inline") {
      const stageCell = Math.floor(stageFont * ratio);
      const metrics = cellAt(sized(stageCell), dpr);
      const high = metrics?.height ?? (screen.offsetHeight / term.rows) * dpr;
      // The answer strip borrows its height from the stage: the pane keeps
      // its size under it, since a resize redraws the dialog it answers.
      const lent = stripBox.current?.offsetHeight ?? 0;
      const target = clampFit(
        Math.floor((width * dpr) / stageCell),
        Math.floor(((box.clientHeight + lent - PAD * 2) * dpr) / high),
      );
      const before = fitTarget.current;
      fitTarget.current = target;
      if (before && (before.cols !== target.cols || before.rows !== target.rows)) {
        if (refitTimer.current !== null) clearTimeout(refitTimer.current);
        refitTimer.current = setTimeout(() => {
          refitTimer.current = null;
          streamRef.current?.refit();
        }, REFIT_DELAY);
      }
    }
    let cell = Math.floor(most * ratio);
    if (fit) cell = Math.min(cell, Math.floor((width * dpr) / term.cols));
    cell = Math.max(least, cell);
    // Rows are whole pixels, so their height does not grow evenly with the
    // font: a guess from the height now can flip between two sizes on every
    // pass. Step down to the largest cell whose own rows fit the room.
    const rowsHigh = (cells: number) => {
      const its = cellAt(sized(cells), dpr);
      if (its) return (its.height * term.rows) / dpr;
      return (screen.offsetHeight / current) * sized(cells);
    };
    while (cell > least && rowsHigh(cell) > room) cell -= 1;
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
    if (stage && view === "inline") setCapped(cell >= Math.floor(most * ratio));
  }, [host, box, view, scale, stage, stageFont]);

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
      term.textarea?.setAttribute("aria-label", watch ? `Tail of ${label}` : `Type to ${label}`);
      // Sized first, so a Fit's first open already knows the stage's room.
      layout();
      streamRef.current?.start();
    }
    layout();
    if (focusNext.current) {
      focusNext.current = false;
      term.focus();
    } else if (typeWhenLive.current && !term.options.disableStdin) {
      typeWhenLive.current = false;
      term.focus();
    }
  }, [box, ready, host, label, layout, watch]);

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

  const openHistory = useCallback(() => {
    if (historyAsk.current) return;
    const ask = new AbortController();
    historyAsk.current = ask;
    setHistory("reading");
    void readHistory(sessionId, ask.signal, peer).then((read) => {
      if (ask.signal.aborted) return;
      historyAsk.current = null;
      if (read.ok) {
        setHistory({ text: read.text });
      } else {
        setHistory(null);
        showToast("bad", "The scrollback did not load", read.detail);
      }
    });
  }, [sessionId, peer]);

  const retry = useCallback(() => streamRef.current?.retry(), []);
  const done = useCallback(() => termRef.current?.blur(), []);
  const send = useCallback((key: string) => inputRef.current?.keys([key]), []);

  const expand = useCallback(() => {
    // The scrollback is the inline screen's; the large view is live.
    closeHistory();
    if (phone) {
      setSheetTypes(focusNext.current);
      setScale("one");
      setView("sheet");
      return;
    }
    // Fit grows the pane to fill the dialog, which is the point of opening it.
    setScale("fit");
    setView("dialog");
  }, [phone, closeHistory]);

  // Full screen puts the page in full screen, and the Agents view pins the
  // stage's card over it; the layout fits the pane to the screen as the stage
  // grows. The card outlives a switch to another agent, so full screen stays.
  // The browser's Esc leaves it.
  const fullScreenAble = stage && !phone && canFullScreen();
  const toggleFull = useCallback(() => {
    if (full) {
      void document.exitFullscreen().catch(() => {});
      return;
    }
    const term = termRef.current;
    const wasTyping = term?.textarea !== undefined && document.activeElement === term.textarea;
    document.documentElement.requestFullscreen().then(
      () => {
        if (wasTyping) termRef.current?.focus();
      },
      () =>
        showToast(
          "bad",
          "Full screen did not open",
          "The browser refused it. Expand shows the terminal larger.",
        ),
    );
  }, [full]);

  const collapse = useCallback(() => {
    termRef.current?.blur();
    if (popup) onClose?.();
    else setView("inline");
  }, [popup, onClose]);

  useImperativeHandle(
    control,
    () => ({
      type() {
        const term = termRef.current;
        if (watch) return;
        if (phone) {
          focusNext.current = true;
          expand();
        } else if (term?.element && !term.options.disableStdin) term.focus();
        else typeWhenLive.current = true;
      },
    }),
    [phone, expand, watch],
  );

  const ended = status.kind === "ended";
  const screenShown =
    status.kind === "live" ||
    status.kind === "tail" ||
    status.kind === "connecting" ||
    status.kind === "reconnecting" ||
    (ended && size !== null);
  const message = screenShown ? null : ended ? (
    <Message icon={<TerminalIcon />} title="The agent's pane is closed">
      The agent's terminal is gone.
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
      <span className="tx">Waiting for the agent's first screen.</span>
    ) : ended ? (
      <span className="tx">The pane closed {endedAt}. This is its last screen.</span>
    ) : where === "inline" && view !== "inline" ? (
      <span className="tx">The terminal is open in the expanded view.</span>
    ) : peer !== undefined ? (
      <span className="tx">
        <GlobeIcon />
        <span>{phone ? `Read only, on ${peer}` : `Read only: the agent runs on ${peer}`}</span>
      </span>
    ) : status.kind === "tail" ? (
      <span className="tx">
        <GlobeIcon />
        <span>
          {phone ? `Read only, on ${machine}` : `Read only: the agent runs on ${machine}`}
          {" · "}
          <Ago at={status.at} prefix="updated" oldAfter={TAIL_OLD} />
        </span>
      </span>
    ) : (
      <span className="tx">
        <PointerIcon />
        {phone ? "Tap" : "Click"} the screen to type to the agent
      </span>
    );

  // Full screen is already the larger view, so Expand waits for it to end.
  const expandButton =
    screenShown && !ended && view === "inline" && !full ? (
      <button
        type="button"
        className="ib sm"
        title="Expand"
        aria-label="Expand the terminal"
        onClick={expand}
      >
        <ExpandIcon />
      </button>
    ) : null;
  const fitButton =
    stage && !phone && !watch && screenShown && !ended && view === "inline" ? (
      <button
        type="button"
        className="btn sm mir-tog"
        aria-pressed={fitWanted && !unfit}
        title={
          holder
            ? fitWanted && !unfit
              ? "Stop sizing the pane to the stage"
              : "Size the pane to fill the stage"
            : fitWanted && !unfit
              ? "Give the pane back its herdr size"
              : "Size the pane in herdr to fill the stage, until you leave it"
        }
        onClick={() => {
          // After the pane kept its size, a press asks again.
          setUnfit(false);
          if (!unfit || !fitWanted) fitChoiceFor.toggle();
        }}
      >
        <FitIcon />
        Fit pane
      </button>
    ) : null;
  // The stage's font size, on a computer. A press keeps focus where it was,
  // so typing goes on.
  const fontControls =
    stage && !phone && screenShown && view === "inline" ? (
      <span className="mir-font" role="group" aria-label="Font size">
        <button
          type="button"
          className="ib sm"
          title="Smaller text"
          aria-label="Smaller text"
          disabled={stageFont <= STAGE_FONT_MIN}
          onMouseDown={keepFocus}
          onClick={() => setStageFont(stageFont - 1)}
        >
          <FontSmallerIcon />
        </button>
        <button
          type="button"
          className="mir-fsz"
          title={
            stageFont === STAGE_FONT
              ? `Font size ${stageFont} px, the default`
              : `Font size ${stageFont} px. Click for the default, ${STAGE_FONT} px`
          }
          aria-label={
            stageFont === STAGE_FONT
              ? `Font size ${stageFont} px, the default`
              : `Font size ${stageFont} px, back to ${STAGE_FONT} px`
          }
          onMouseDown={keepFocus}
          onClick={() => setStageFont(STAGE_FONT)}
        >
          {stageFont}
        </button>
        <button
          type="button"
          className="ib sm"
          title={
            capped
              ? "Larger text"
              : "The pane fills the stage at this size. Fit pane sizes it to the text"
          }
          aria-label="Larger text"
          disabled={stageFont >= STAGE_FONT_MAX || !capped}
          onMouseDown={keepFocus}
          onClick={() => setStageFont(stageFont + 1)}
        >
          <FontLargerIcon />
        </button>
      </span>
    ) : null;
  // In full screen the way out stays, whatever the stream does.
  const fullButton =
    full || (fullScreenAble && !ended && screenShown && view === "inline") ? (
      <button
        type="button"
        className="ib sm"
        title={full ? "Leave full screen (Esc)" : "Full screen"}
        aria-label={full ? "Leave full screen" : "Full screen"}
        aria-pressed={full}
        onMouseDown={keepFocus}
        onClick={toggleFull}
      >
        {full ? <LeaveFullScreenIcon /> : <FullScreenIcon />}
      </button>
    ) : null;
  const viewControls = (
    <>
      {fontControls}
      {fullButton}
    </>
  );
  // The scrollback is the stage's, while the screen shows inline.
  const canHistory = stage && !tail && screenShown && !ended && view === "inline";
  const historyButton =
    canHistory && history === null ? (
      <button
        type="button"
        className="ib sm"
        title="Scroll back"
        aria-label="Read the pane's scrollback"
        onClick={openHistory}
      >
        <HistoryIcon />
      </button>
    ) : null;
  const historyOpen = canHistory && history !== null;
  const shownHistory = historyOpen && history !== "reading" ? history : null;
  // A click anywhere on the screen types, not only on the pane's own cells:
  // the stage is often wider and taller than the pane it shows.
  const typeFromScreen = (event: ReactMouseEvent<HTMLDivElement>) => {
    const term = termRef.current;
    if (watch || phone || typing || historyOpen) return;
    if (!term?.element || term.options.disableStdin || !pressTypes(event, term.element)) return;
    // Else the press moves focus to the body right after the screen takes it.
    event.preventDefault();
    term.focus();
  };

  // Esc goes back to live wherever focus is, unless a dialog takes it. The
  // scrollback's own screen catches it first and marks it taken.
  useEffect(() => {
    if (!historyOpen) return;
    const onKey = (event: KeyboardEvent) => {
      if (event.key !== "Escape" || event.defaultPrevented) return;
      if (shortcutsBlocked()) return;
      event.preventDefault();
      closeHistory();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [historyOpen, closeHistory]);
  const screen = (
    <div
      className="mir-s"
      role="group"
      aria-label={`${watch ? "Tail" : "Live terminal"} for ${label}`}
      onMouseDown={typeFromScreen}
      // Scrolling up on the stage reads back, as it would in the terminal.
      onWheel={(event) => {
        if (event.deltaY < 0 && canHistory && history === null && !typing) openHistory();
      }}
    >
      <div className="mir-slot" ref={setInlineBox} />
      {shownHistory && size ? (
        <HistoryScreen
          text={shownHistory.text}
          cols={size.cols}
          rows={size.rows}
          fontSize={font}
          label={label}
          onClose={closeHistory}
        />
      ) : null}
      {canHistory && history === "reading" ? (
        <div className="mir-veil">
          <span className="mir-pill">
            <span className="mir-spin" />
            Reading the scrollback
          </span>
        </div>
      ) : null}
      {stage && view === "dialog" ? (
        <div className="mir-veil">
          <span className="mir-pill">In the expanded view</span>
        </div>
      ) : (
        veil
      )}
      {phone && live && !shownHistory ? (
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
  );
  // The stage's footer: what a click does on the left, the pane's size and
  // the expand button on the right, Done while typing.
  const stageFooter = typing ? (
    <>
      <span className="tx">
        <KeyboardIcon />
        Typing goes to the agent. Board shortcuts are paused.
      </span>
      <span className="mir-fr">
        {onSwitch ? (
          <span className="mir-sw">
            <kbd>⌥J</kbd>
            <kbd>⌥K</kbd>
            switch agents
          </span>
        ) : null}
        {viewControls}
        <button type="button" className="btn sm" onClick={done}>
          Done
        </button>
      </span>
    </>
  ) : historyOpen ? (
    <>
      <span className="tx">
        <HistoryIcon />
        {phone || full
          ? "Scrollback, read-only"
          : "Scrollback, read-only. Esc goes back to the live screen."}
      </span>
      <span className="mir-fr">
        {viewControls}
        <button type="button" className="btn sm primary" onClick={closeHistory}>
          Back to live
        </button>
      </span>
    </>
  ) : !screenShown ? (
    // In full screen with no screen: only the way out.
    <span className="mir-fr">{viewControls}</span>
  ) : (
    <>
      {footer("inline")}
      <span className="mir-fr">
        {sizeText ? <span className="mir-sz">{sizeText}</span> : null}
        {historyButton}
        {viewControls}
        {fitButton}
        {expandButton}
      </span>
    </>
  );

  return (
    // A popup has no inline view: only its dialog or sheet, which a portal
    // puts over the page.
    <div className={popup ? "mir-popup" : `mir${stage ? " stage" : ""}${state}`}>
      {stage || popup ? null : (
        <div className="mir-h">
          <StatusMark status={status} />
          {typing && view === "inline" ? <span className="lab sm acc">Typing</span> : null}
          {screenShown && sizeText && view === "inline" ? (
            <span className="mir-sz">{sizeText}</span>
          ) : null}
          <span className="sp" />
          {expandButton}
        </div>
      )}
      {popup ? null : message}
      {screenShown && (view === "inline" || stage) ? screen : null}
      {screenShown && stage ? (
        <div className="mir-strip" ref={stripBox}>
          {strip}
        </div>
      ) : null}
      {(screenShown || full) && !popup ? (
        <div className="mir-f">{stage ? stageFooter : footer("inline")}</div>
      ) : null}
      {view === "dialog" ? (
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
              aria-label={`${watch ? "Tail" : "Live terminal"} for ${label}`}
              hidden={!screenShown}
              onMouseDown={typeFromScreen}
            >
              <div className="mir-slot" ref={setBigBox} data-trap-skip="" />
              {veil}
            </div>
            <div className="mir-f">{footer("big")}</div>
          </div>
        </Expanded>
      ) : null}
      {view === "sheet" ? (
        // It fills the phone's screen, so it has no backdrop.
        <Modal
          backdrop={null}
          className={`mir-sheet${state}`}
          label={`Terminal for ${label}`}
          onClose={collapse}
          initialFocus={sheetTypes ? "child" : undefined}
        >
          <div className="mir-sheet-h">
            <button type="button" className="ib" aria-label="Back" onClick={collapse}>
              <ChevronLeftIcon />
            </button>
            <span className="mir-sheet-t">{heading}</span>
            <span className="sp" />
            <ScaleSwitch scale={scale} onScale={setScale} />
            {/* A popup's sheet is its only view, so it keeps its way on. */}
            {popup ? actions : null}
          </div>
          {message}
          <div
            className={`mir-sheet-b ${scale}`}
            ref={setBigBox}
            role="group"
            aria-label={`${watch ? "Tail" : "Live terminal"} for ${label}`}
            hidden={!screenShown}
            data-trap-skip=""
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
          {watch ? null : (
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
          )}
        </Modal>
      ) : null}
    </div>
  );
}

// The pane's scrollback over the live screen, at its size and font, so the
// last rows line up with the screen under it. A terminal of its own, read
// only: the live one keeps drawing frames under it.
function HistoryScreen({
  text,
  cols,
  rows,
  fontSize,
  label,
  onClose,
}: {
  text: string;
  cols: number;
  rows: number;
  fontSize: number;
  label: string;
  onClose: () => void;
}) {
  const [box, setBox] = useState<HTMLDivElement | null>(null);
  const termRef = useRef<Terminal | null>(null);
  // The live screen's shape, read when xterm has loaded, so a change while it
  // loads is not lost.
  const shape = useRef({ cols, rows, fontSize });

  useEffect(() => {
    if (!box) return;
    let gone = false;
    let term: Terminal | null = null;
    // The live screen's renderer too, so the cells measure the same.
    void Promise.all([import("@xterm/xterm"), import("@xterm/addon-webgl")])
      .then(([xterm, webgl]) => {
        if (gone) return;
        term = new xterm.Terminal({
          ...OPTIONS,
          ...shape.current,
          scrollback: HISTORY_ROWS,
          // herdr splits rows with a bare newline.
          convertEol: true,
          cursorBlink: false,
          theme: {
            ...THEME,
            scrollbarSliderBackground: "#ffffff26",
            scrollbarSliderHoverBackground: "#ffffff40",
            scrollbarSliderActiveBackground: "#ffffff59",
          },
        });
        // xterm keeps the keys it reads to itself, so Esc is caught here.
        term.attachCustomKeyEventHandler((event) => {
          if (event.key !== "Escape") return true;
          event.preventDefault();
          if (event.type === "keydown") onClose();
          return false;
        });
        term.open(box);
        openRenderer(term, webgl.WebglAddon);
        term.textarea?.setAttribute("aria-label", `Scrollback of ${label}`);
        termRef.current = term;
        const opened = term;
        // No cursor, and no empty row after the last one.
        opened.write(`\x1b[?25l${text.replace(/\n$/, "")}`, () => {
          opened.scrollToBottom();
          opened.focus();
        });
      })
      .catch(() => {
        if (gone) return;
        showToast("bad", "The scrollback did not load", "The terminal did not load.");
        onClose();
      });
    return () => {
      gone = true;
      term?.dispose();
      termRef.current = null;
    };
  }, [box, text, label, onClose]);

  useEffect(() => {
    shape.current = { cols, rows, fontSize };
    const term = termRef.current;
    if (!term) return;
    if (term.cols !== cols || term.rows !== rows) term.resize(cols, rows);
    if (term.options.fontSize !== fontSize) term.options.fontSize = fontSize;
  }, [cols, rows, fontSize]);

  return (
    <div className="mir-hist">
      <div className="mir-hx" ref={setBox} />
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
// close it (Escape typed to the agent stays with the agent), and Tab wraps
// between the dialog's own controls, past the terminal.
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
  const close = useRef<HTMLButtonElement>(null);
  return (
    <Modal
      backdrop="dialog-backdrop mir-backdrop"
      className="mir-dlg"
      label={`Terminal for ${label}`}
      onClose={onClose}
      initialFocus={close}
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
    </Modal>
  );
}
