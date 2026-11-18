import { Fragment } from "react";
import type { ReactNode } from "react";

import { parseBlocks, parseInline } from "./markdown";
import type { Block, Inline } from "./markdown";

function inlines(parts: Inline[]): ReactNode {
  return parts.map((part, index) => {
    switch (part.kind) {
      case "text":
        return <Fragment key={index}>{part.text}</Fragment>;
      case "br":
        return <br key={index} />;
      case "code":
        return <code key={index}>{part.text}</code>;
      case "strong":
        return <strong key={index}>{inlines(part.children)}</strong>;
      case "em":
        return <em key={index}>{inlines(part.children)}</em>;
      case "link":
        return (
          <a key={index} href={part.href} target="_blank" rel="noreferrer">
            {inlines(part.children)}
          </a>
        );
    }
  });
}

// A list item of one paragraph is its words alone, so a short list stays tight.
function item(blocks: Block[]): ReactNode {
  const [only] = blocks;
  if (blocks.length === 1 && only?.kind === "p") return inlines(parseInline(only.text));
  return blocks.map(block);
}

function block(value: Block, index: number): ReactNode {
  switch (value.kind) {
    case "p":
      return <p key={index}>{inlines(parseInline(value.text))}</p>;
    case "h":
      return (
        <p key={index} className="md-h">
          {inlines(parseInline(value.text))}
        </p>
      );
    case "code":
      return (
        <pre key={index}>
          <code>{value.text}</code>
        </pre>
      );
    case "quote":
      return <blockquote key={index}>{value.blocks.map(block)}</blockquote>;
    case "list": {
      const items = value.items.map((blocks, at) => <li key={at}>{item(blocks)}</li>);
      return value.ordered ? (
        <ol key={index} start={value.start === 1 ? undefined : value.start}>
          {items}
        </ol>
      ) : (
        <ul key={index}>{items}</ul>
      );
    }
  }
}

// A chat message's words, with the Markdown it uses shown as Markdown.
export function Markdown({ text, className }: { text: string; className: string }) {
  return <div className={className}>{parseBlocks(text).map(block)}</div>;
}
