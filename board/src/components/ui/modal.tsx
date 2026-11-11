import { useEffect, useEffectEvent, useLayoutEffect, useRef } from "react";
import type { KeyboardEvent, ReactNode, RefObject } from "react";
import { createPortal } from "react-dom";

import { holdInert } from "../../lib/inert";

// Every modal on the board is this one dialog. It owns what each used to
// copy: Escape and the backdrop close it, Tab stays on its own controls, the
// page behind it is inert, and focus goes back to what opened it on close.

// The controls Tab stops on, in order, and what a dialog's opener can be.
const CONTROLS = 'a[href], button, input, select, textarea, [contenteditable="true"], [tabindex]';
// What a press can make the opener: a control the owner can reach, not a
// panel or a dialog that only script focuses.
const PRESSABLE =
  'a[href], button, input, select, textarea, [contenteditable="true"], [tabindex]:not([tabindex="-1"])';

// The open modals, the newest last: only the top one takes a stray Escape.
const stack: object[] = [];

// The controls focused last outside a menu, the newest last, and the last
// one focused anywhere. A dialog opened from a menu item, or from a popover
// that closes as it opens, gives focus back to the control that opened the
// menu or popover: the item is gone.
const recent: HTMLElement[] = [];
let last: HTMLElement | null = null;
// The control the pointer pressed last, until a key moves on from it. Safari,
// and Firefox on macOS, do not focus a button that is clicked, so a dialog it
// opens finds its opener here.
let pressed: HTMLElement | null = null;
// Counts the presses, so a wait from an earlier one never forgets a later one.
let presses = 0;
if (typeof document !== "undefined") {
  document.addEventListener(
    "pointerdown",
    (event) => {
      const target = event.target instanceof Element ? event.target : null;
      presses += 1;
      // The nearest control the owner can reach, out past a script-only part
      // inside it, but never out of a dialog: a press on its text is no
      // control's.
      let control = target?.closest<HTMLElement>(CONTROLS) ?? null;
      while (control && !control.matches(PRESSABLE)) {
        control = control.matches('[role="dialog"], [aria-modal="true"]')
          ? null
          : (control.parentElement?.closest<HTMLElement>(CONTROLS) ?? null);
      }
      pressed = control && !control.closest('[role="menu"]') ? control : null;
    },
    true,
  );
  document.addEventListener(
    "keydown",
    () => {
      pressed = null;
    },
    true,
  );
  // Only the press's own click opens a dialog from it: after that task, a
  // dialog that opens (a stream event, a timer) is not the button's. A press
  // with no click yet is kept a moment: iOS Safari can send a tap's click in
  // a later task than the lift.
  const forget = (wait: number) => () => {
    const press = presses;
    setTimeout(() => {
      if (presses === press) pressed = null;
    }, wait);
  };
  document.addEventListener("click", forget(0), true);
  document.addEventListener("pointerup", forget(1000), true);
  document.addEventListener("pointercancel", forget(0), true);
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
  // A click that left focus on the page, or on a part around the control
  // (Safari, and Firefox on macOS, never focus a clicked button): the control
  // it pressed.
  if (
    active instanceof HTMLElement &&
    active !== pressed &&
    pressed?.isConnected &&
    active.contains(pressed)
  ) {
    return pressed;
  }
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

// A part marked `data-trap-skip` (a live terminal) is never one of the ends
// Tab wraps between.
function tabStops(dialog: HTMLElement): HTMLElement[] {
  return [...dialog.querySelectorAll<HTMLElement>(CONTROLS)].filter(
    (element) =>
      !(element as HTMLButtonElement).disabled &&
      Number(element.getAttribute("tabindex") ?? 0) >= 0 &&
      !(element instanceof HTMLInputElement && element.type === "hidden") &&
      !element.closest("[hidden], [inert], [data-trap-skip]"),
  );
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
    const before: Element[] = [];
    for (let at = root.previousElementSibling; at; at = at.previousElementSibling) before.push(at);
    const release = holdInert(before);
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
      release();
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
