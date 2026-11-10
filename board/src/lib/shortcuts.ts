// Whether a modal dialog is on screen: one marked `aria-modal`, or a native
// <dialog> opened with showModal().
function modalOpen(doc: Document): boolean {
  if (doc.querySelector('[aria-modal="true"]')) return true;
  return [...doc.querySelectorAll("dialog[open]")].some((dialog) => {
    try {
      return dialog.matches(":modal");
    } catch {
      // An engine that does not know `:modal`: an open dialog counts as one.
      return true;
    }
  });
}

// Whether a key is being typed into a field: an input, a text area, a select
// or anything editable, such as the Ask editor or a terminal.
function typingInto(target: EventTarget | null): boolean {
  if (!(target instanceof Element)) return false;
  if (target instanceof HTMLElement && target.isContentEditable) return true;
  return (
    target.closest('input, textarea, select, [contenteditable]:not([contenteditable="false"])') !==
    null
  );
}

// The one check every board-wide key handler makes first. A key belongs to
// the modal on screen, so no shortcut acts behind it. Given the event, a key
// typed into a field is the field's own too.
export function shortcutsBlocked(event?: Event): boolean {
  if (modalOpen(document)) return true;
  return event !== undefined && typingInto(event.target);
}
