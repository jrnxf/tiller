import type { Editor } from "@tiptap/core";

// One project the owner can mention. This file holds what the dialog needs
// before the editor loads, so the dialog imports no editor code.
export interface MentionOption {
  key: string;
  name: string;
  color: string;
  // The paired peer's label for a project another host owns; null for this host's.
  host: string | null;
  // Why this project cannot take an ask now; null when it can.
  off: string | null;
}

// Takes every chip of this project out of the text.
export function removeMention(editor: Editor, key: string) {
  const { tr, doc } = editor.state;
  // Last first, so the earlier positions hold while deleting.
  const at: { from: number; to: number }[] = [];
  doc.descendants((node, pos) => {
    if (node.type.name === "mention" && node.attrs.id === key) {
      at.unshift({ from: pos, to: pos + node.nodeSize });
    }
  });
  for (const { from, to } of at) tr.delete(from, to);
  editor.view.dispatch(tr);
}
