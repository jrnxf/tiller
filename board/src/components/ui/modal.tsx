import { useEffect, useEffectEvent, useLayoutEffect, useRef } from "react";
import type { KeyboardEvent, ReactNode, RefObject } from "react";
import { createPortal } from "react-dom";

// Every modal on the board is this one dialog. It owns what each used to
// copy: Escape and the backdrop close it, Tab stays on its own controls, the
// page behind it is inert, and focus goes back to what opened it on close.

// The open modals, the newest last: only the top one takes a stray Escape.
const stack: object[] = [];

// The controls focused last outside a menu, the newest last, and the last
// one focused anywhere. A dialog opened from a menu item, or from a popover
// that closes as it opens, gives focus back to the control that opened the
// menu or popover: the item is gone.
const recent: HTMLElement[] = [];
let last: HTMLElement | null = null;
if (typeof document !== "undefined") {
  document.addEventListener(
    "focusin",
    (event) => {
      const target = event.target;
      if (!(target instanceof HTMLElement)) return;
      last = target;
      if (target.closest('[role="menu"]')) return;
      const at = recent.indexOf(target);
      if (at !== -1) recent.splice(at, 1);
      recent.push(target);
      if (recent.length > 8) recent.shift();
    },
    true,
  );
}

function openerNow(): HTMLElement | null {
  const active = document.activeElement;
  const inMenu = active instanceof HTMLElement && active.closest('[role="menu"]') !== null;
  if (active instanceof HTMLElement && active !== document.body && !inMenu) return active;
  // Focus fell to the page because the control that had it was removed, not
  // because the owner clicked the page.
  const dropped = active === document.body && last?.isConnected === false;
  if (!inMenu && !dropped) return null;
  for (let at = recent.length - 1; at >= 0; at -= 1) {
    const element = recent[at];
    if (element?.isConnected) return element;
  }
  return null;
}

// The controls Tab stops on, in order. A part marked `data-trap-skip` (a live
// terminal) is never one of the ends Tab wraps between.
const CONTROLS = 'a[href], button, input, select, textarea, [contenteditable="true"], [tabindex]';

function tabStops(dialog: HTMLElement): HTMLElement[] {
  return [...dialog.querySelectorAll<HTMLElement>(CONTROLS)].filter(
    (element) =>
      !(element as HTMLButtonElement).disabled &&
      Number(element.getAttribute("tabindex") ?? 0) >= 0 &&
      !(element instanceof HTMLInputElement && element.type === "hidden") &&
      !element.closest("[hidden], [inert], [data-trap-skip]"),
  );
}

// Marks each element inert, and records it in `made`. One that holds a part
// marked `data-modal-exempt` (the toast, which must still be announced) stays
// live itself, and its other children are marked instead.
function makeInert(elements: Iterable<Element>, made: Element[]) {
  for (const element of elements) {
    if (element.hasAttribute("inert") || element.hasAttribute("data-modal-exempt")) continue;
    if (element.querySelector("[data-modal-exempt]")) {
      makeInert(element.children, made);
    } else {
      element.setAttribute("inert", "");
      made.push(element);
    }
  }
}

export function Modal({
  backdrop = "dialog-backdrop",
  className,
  label,
  labelledBy,
  describedBy,
  dismissable = true,
  onClose,
  onEscape,
  initialFocus,
  onKeyDownCapture,
  children,
}: {
  // The backdrop's class, or null for a dialog that fills the screen itself.
  backdrop?: string | null;
  className: string;
  label?: string;
  labelledBy?: string;
  describedBy?: string;
  // False while a send is in flight: the backdrop and Escape keep the dialog.
  dismissable?: boolean;
  onClose: () => void;
  // What Escape does when it is not a plain close.
  onEscape?: () => void;
  // Takes focus on open; else the first control does. "child" leaves it to a
  // child that focuses itself, such as an editor.
  initialFocus?: RefObject<HTMLElement | null> | "child";
  onKeyDownCapture?: (event: KeyboardEvent<HTMLDivElement>) => void;
  children: ReactNode;
}) {
  const outer = useRef<HTMLDivElement>(null);
  const dialog = useRef<HTMLDivElement>(null);

  const escape = () => {
    if (dismissable) (onEscape ?? onClose)();
  };
  const strayEscape = useEffectEvent(escape);

  useLayoutEffect(() => {
    const box = dialog.current;
    const root = outer.current ?? box;
    if (!box || !root) return;
    // Read before this dialog takes focus. When one dialog takes the place of
    // another, the first has given focus back by now, so both return to the
    // same control.
    const opener = openerNow();
    const token = {};
    stack.push(token);
    // Everything on the page before this dialog: the board, and any dialog
    // under it. What opens later (a list, a menu, a toast) stays usable.
    const made: Element[] = [];
    const before: Element[] = [];
    for (let at = root.previousElementSibling; at; at = at.previousElementSibling) before.push(at);
    makeInert(before, made);
    // Escape with focus outside the dialog, such as on the page body.
    const onKey = (event: globalThis.KeyboardEvent) => {
      if (event.key !== "Escape" || event.defaultPrevented || stack.at(-1) !== token) return;
      if (event.target instanceof Node && box.contains(event.target)) return;
      event.preventDefault();
      strayEscape();
    };
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("keydown", onKey);
      stack.splice(stack.indexOf(token), 1);
      for (const element of made) element.removeAttribute("inert");
      // Unless what closed it moved focus on purpose.
      const active = document.activeElement;
      const lost = !active || active === document.body || box.contains(active);
      if (lost && opener?.isConnected) opener.focus();
    };
  }, []);

  useEffect(() => {
    const box = dialog.current;
    if (!box || initialFocus === "child") return;
    // A child that took focus as it mounted, such as a terminal opened by a
    // tap that means "type", keeps it.
    if (box.contains(document.activeElement)) return;
    (initialFocus?.current ?? tabStops(box)[0] ?? box).focus();
  }, [initialFocus]);

  const panel = (
    <div
      ref={dialog}
      className={className}
      role="dialog"
      aria-modal="true"
      aria-label={label}
      aria-labelledby={labelledBy}
      aria-describedby={describedBy}
      // A click on the dialog's own text keeps focus in it.
      tabIndex={-1}
      onMouseDown={backdrop === null ? undefined : (event) => event.stopPropagation()}
      onKeyDownCapture={onKeyDownCapture}
      onKeyDown={(event) => {
        if (event.key === "Escape") {
          event.preventDefault();
          event.stopPropagation();
          escape();
        } else if (event.key === "Tab") {
          const stops = tabStops(event.currentTarget);
          const [first, end] = [stops[0], stops.at(-1)];
          const active = document.activeElement;
          if (!first || !end) {
            // Everything is disabled while an action is sent: focus stays.
            event.preventDefault();
          } else if (event.shiftKey && (active === first || active === event.currentTarget)) {
            event.preventDefault();
            end.focus();
          } else if (!event.shiftKey && active === end) {
            event.preventDefault();
            first.focus();
          }
        }
      }}
    >
      {children}
    </div>
  );

  return createPortal(
    backdrop === null ? (
      panel
    ) : (
      <div
        ref={outer}
        className={backdrop}
        onMouseDown={() => {
          if (dismissable) onClose();
        }}
      >
        {panel}
      </div>
    ),
    document.body,
  );
}
