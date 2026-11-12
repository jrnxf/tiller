// Who holds each element inert. A modal and the phone drawer can both make
// the same part of the page inert, and they let go in any order: the page
// comes back only when the last of them lets go. An element made inert by
// anything else (its own markup) is never held, so it is never freed here.
const holds = new Map<Element, number>();

// Marks each element inert, and returns what lets go of them. One that holds a
// part marked `data-modal-exempt` (the toast, which must still be announced)
// stays live itself, and its other children are marked instead.
export function holdInert(elements: Iterable<Element>): () => void {
  const held: Element[] = [];
  hold(elements, held);
  let released = false;
  return () => {
    if (released) return;
    released = true;
    for (const element of held) {
      const count = (holds.get(element) ?? 1) - 1;
      if (count > 0) {
        holds.set(element, count);
      } else {
        holds.delete(element);
        element.removeAttribute("inert");
      }
    }
  };
}

// Marks the whole page inert but `element` and what holds it: the rest of
// each parent's children, up to the body. A dialog, a menu or a toast that
// opens on the body later stays live.
export function holdInertOutside(element: Element): () => void {
  const others: Element[] = [];
  for (let at = element; at.parentElement && at !== document.body; at = at.parentElement) {
    for (const other of at.parentElement.children) if (other !== at) others.push(other);
  }
  return holdInert(others);
}

function hold(elements: Iterable<Element>, held: Element[]) {
  for (const element of elements) {
    if (element.hasAttribute("data-modal-exempt")) continue;
    const count = holds.get(element);
    if (count === undefined && element.hasAttribute("inert")) continue;
    if (element.querySelector("[data-modal-exempt]")) {
      hold(element.children, held);
      continue;
    }
    holds.set(element, (count ?? 0) + 1);
    element.setAttribute("inert", "");
    held.push(element);
  }
}
