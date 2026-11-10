import { Editor, mergeAttributes } from "@tiptap/core";
import type { JSONContent } from "@tiptap/core";
import Document from "@tiptap/extension-document";
import Mention from "@tiptap/extension-mention";
import Paragraph from "@tiptap/extension-paragraph";
import Text from "@tiptap/extension-text";
import { Placeholder } from "@tiptap/extensions";
import { useEffect, useRef } from "react";
import type { RefObject } from "react";

import type { AskPiece } from "../../lib/ask-plan";
import type { MentionOption } from "./ask-mentions";

// The text and chips of the document, in order. A new paragraph reads as a line break.
function piecesOf(doc: JSONContent): AskPiece[] {
  const pieces: AskPiece[] = [];
  (doc.content ?? []).forEach((paragraph, index) => {
    if (index > 0) pieces.push({ kind: "text", text: "\n" });
    for (const node of paragraph.content ?? []) {
      if (node.type === "mention") {
        const key = String(node.attrs?.id ?? "");
        pieces.push({ kind: "mention", key, name: String(node.attrs?.label ?? key) });
      } else if (node.type === "text" && node.text) {
        pieces.push({ kind: "text", text: node.text });
      }
    }
  });
  return pieces;
}

// Fuzzy enough for project names: the typed letters appear in order.
function matches(name: string, query: string): boolean {
  const lower = name.toLowerCase();
  let at = 0;
  for (const letter of query.toLowerCase()) {
    at = lower.indexOf(letter, at) + 1;
    if (at === 0) return false;
  }
  return true;
}

interface Popup {
  onStart: (props: PopupProps) => void;
  onUpdate: (props: PopupProps) => void;
  onKeyDown: (props: { event: KeyboardEvent }) => boolean;
  onExit: () => void;
}

interface PopupProps {
  items: MentionOption[];
  command: (attrs: { id: string; label: string }) => void;
  clientRect?: (() => DOMRect | null) | null;
}

const POP_WIDTH = 340;
const POP_HEIGHT = 260;

const enabled = (item: MentionOption) => item.off === null;

const firstEnabled = (items: MentionOption[]) => Math.max(0, items.findIndex(enabled));

// The list of projects under the caret: plain DOM, so the editor owns the
// keys and the list needs no React tree of its own.
function popupList(onOpen: (open: boolean) => void): Popup {
  let el: HTMLDivElement | null = null;
  let current: PopupProps | null = null;
  let index = 0;

  const choose = (at: number) => {
    const item = current?.items[at];
    if (item && enabled(item)) current?.command({ id: item.key, label: item.name });
  };
  const place = () => {
    const rect = current?.clientRect?.();
    if (!el || !rect) return;
    const left = Math.max(8, Math.min(rect.left, window.innerWidth - POP_WIDTH - 8));
    const below = rect.bottom + 4;
    const flip = below + POP_HEIGHT > window.innerHeight && rect.top > POP_HEIGHT;
    el.style.left = `${left}px`;
    el.style.top = flip ? "" : `${below}px`;
    el.style.bottom = flip ? `${window.innerHeight - rect.top + 4}px` : "";
  };
  const draw = () => {
    if (!el || !current) return;
    el.replaceChildren();
    if (current.items.length === 0) {
      const empty = document.createElement("div");
      empty.className = "ask-pop-empty";
      empty.textContent = "No project matches";
      el.append(empty);
      return;
    }
    current.items.forEach((item, at) => {
      const row = document.createElement("div");
      row.className = "ask-pop-item";
      row.setAttribute("role", "option");
      row.setAttribute("aria-selected", String(at === index));
      row.setAttribute("aria-disabled", String(!enabled(item)));
      row.dataset.disabled = String(!enabled(item));
      const dot = document.createElement("span");
      dot.className = "ask-dot";
      dot.style.setProperty("--pc", item.color);
      const name = document.createElement("span");
      name.className = "ask-pop-name";
      name.textContent = item.name;
      row.append(dot, name);
      if (item.off) {
        const off = document.createElement("span");
        off.className = "ask-pop-off";
        off.textContent = item.off;
        row.title = item.off;
        row.append(off);
      } else if (item.host) {
        const host = document.createElement("span");
        host.className = "ask-tag";
        host.textContent = item.host;
        row.append(host);
      }
      // Keep the caret in the editor while the row is pressed.
      row.addEventListener("mousedown", (event) => event.preventDefault());
      row.addEventListener("click", () => choose(at));
      el?.append(row);
    });
    el.querySelector('[aria-selected="true"]')?.scrollIntoView?.({ block: "nearest" });
  };
  const show = (props: PopupProps, fresh: boolean) => {
    current = props;
    if (fresh || index >= props.items.length) index = firstEnabled(props.items);
    draw();
    place();
  };

  return {
    onStart(props) {
      el = document.createElement("div");
      el.className = "ask-pop";
      el.setAttribute("role", "listbox");
      el.setAttribute("aria-label", "Projects");
      document.body.append(el);
      onOpen(true);
      show(props, true);
    },
    onUpdate: (props) => show(props, true),
    onKeyDown({ event }) {
      const items = current?.items ?? [];
      if (event.key === "ArrowDown" || event.key === "ArrowUp") {
        if (items.length === 0) return true;
        const step = event.key === "ArrowDown" ? 1 : -1;
        index = (index + step + items.length) % items.length;
        draw();
        return true;
      }
      if (event.key === "Enter" || event.key === "Tab") {
        if (items.length === 0) return false;
        choose(index);
        return true;
      }
      return false;
    },
    onExit() {
      el?.remove();
      el = null;
      current = null;
      onOpen(false);
    },
  };
}

interface Props {
  options: MentionOption[];
  // Keys already mentioned, which the list leaves out.
  used: string[];
  // Read only once the sends have begun.
  locked: boolean;
  placeholder: string;
  labelledBy: string;
  editorRef: RefObject<Editor | null>;
  onChange: (pieces: AskPiece[]) => void;
  onSubmit: () => void;
  // The list of projects opened or closed.
  onListOpen: (open: boolean) => void;
}

// The ask text with @project chips: a Tiptap editor with the Mention node.
// Enter sends and Shift+Enter adds a line, except while the list is open,
// where Enter picks the project.
export function AskEditor(props: Props) {
  const host = useRef<HTMLDivElement>(null);
  const latest = useRef(props);
  useEffect(() => {
    latest.current = props;
  });
  const { editorRef } = props;

  useEffect(() => {
    const element = host.current;
    if (!element) return;
    let listOpen = false;
    const editor = new Editor({
      element,
      extensions: [
        Document,
        Paragraph,
        Text,
        Placeholder.configure({ placeholder: latest.current.placeholder }),
        Mention.configure({
          // A chip carries the project name alone, with no "@".
          renderHTML: ({ node }) => [
            "span",
            mergeAttributes({ class: "ask-chip", "data-type": "mention" }),
            String(node.attrs.label ?? node.attrs.id),
          ],
          renderText: ({ node }) => String(node.attrs.label ?? node.attrs.id),
          suggestion: {
            char: "@",
            allowedPrefixes: null,
            items: ({ query }) => {
              const { options, used } = latest.current;
              return options.filter(
                (option) => !used.includes(option.key) && matches(option.name, query),
              );
            },
            render: () =>
              popupList((open) => {
                listOpen = open;
                latest.current.onListOpen(open);
              }),
          },
        }),
      ],
      editorProps: {
        attributes: {
          role: "textbox",
          "aria-multiline": "true",
          "aria-labelledby": latest.current.labelledBy,
        },
        handleKeyDown: (_view, event) => {
          if (event.key !== "Enter" || event.isComposing || listOpen) return false;
          if (event.shiftKey) return editor.commands.splitBlock();
          latest.current.onSubmit();
          return true;
        },
      },
      onUpdate: ({ editor: changed }) => latest.current.onChange(piecesOf(changed.getJSON())),
    });
    editorRef.current = editor;
    editor.commands.focus("end");
    return () => {
      editor.destroy();
      editorRef.current = null;
    };
  }, [editorRef]);

  useEffect(() => {
    editorRef.current?.setEditable(!props.locked);
  }, [props.locked, editorRef]);

  return <div className="ask-ed" ref={host} />;
}
