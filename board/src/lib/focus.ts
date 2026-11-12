// No control holds focus, as when the one that did left the page.
export function focusIsFree(): boolean {
  const now = document.activeElement;
  return !now || now === document.body;
}
