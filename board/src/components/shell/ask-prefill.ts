import type { JSONContent } from "@tiptap/core";

import { createStore } from "../../lib/store";

// An ask the Ask box opens with: the project it goes to (its project key) and
// the words. "Ask again" on a reply sets it, which opens the box; closing the
// box clears it.
export interface AskPrefill {
  project: string;
  text: string;
}

export const askPrefillStore = createStore<AskPrefill | null>(null);

// The footer an ask to several projects carries (lib/ask-plan.ts). Asking
// again goes to one project, so it is left off.
const FOOTER = /\n\nAlso asked of: [^\n]*$/;

// The editor document for a prefill: the project's chip, then the words, one
// paragraph per line. `name` is the project's shown name; with none (the
// project is not listed) the words come alone, for the owner to pick one.
export function prefillDoc(prefill: AskPrefill, name: string | undefined): JSONContent {
  const lines = prefill.text.replace(FOOTER, "").split("\n");
  return {
    type: "doc",
    content: lines.map((line, index) => {
      const text: JSONContent[] = line === "" ? [] : [{ type: "text", text: line }];
      return {
        type: "paragraph",
        content:
          index === 0 && name !== undefined
            ? [
                { type: "mention", attrs: { id: prefill.project, label: name } },
                { type: "text", text: " " },
                ...text,
              ]
            : text,
      };
    }),
  };
}
