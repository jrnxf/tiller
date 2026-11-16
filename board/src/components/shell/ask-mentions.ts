import type { Editor } from "@tiptap/core";

import { scoreMatch } from "../../lib/commands";

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

// The projects that a typed `@` query finds, the best match first. Fuzzy enough
// for project names: the typed letters appear in order. A name that starts with
// them comes before a name that only holds them ("la": Lantern, then Atlas),
// because Enter takes the first row.
export function rankMentions(options: MentionOption[], query: string): MentionOption[] {
  const ranked: { option: MentionOption; score: number }[] = [];
  for (const option of options) {
    const score = scoreMatch(option.name, query);
    if (score !== null) ranked.push({ option, score });
  }
  // Array.prototype.sort is stable, so equal scores keep the listed order.
  ranked.sort((a, b) => b.score - a.score);
  return ranked.map(({ option }) => option);
}
