import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";

import { lineCount, parseAnsi, tidy } from "./ansi";

// "01M6KTNA...F632Z": enough of a session id to recognise it.
function shortId(id: string): string {
  return id.length > 14 ? `${id.slice(0, 8)}…${id.slice(-5)}` : id;
}

function CopyChip({ sessionId }: { sessionId: string }) {
  const [copied, setCopied] = useState(false);
  useEffect(() => {
    if (!copied) return;
    const timer = window.setTimeout(() => setCopied(false), 1600);
    return () => window.clearTimeout(timer);
  }, [copied]);
  const copy = () => {
    navigator.clipboard?.writeText(sessionId).then(
      () => setCopied(true),
      () => undefined,
    );
  };
  return (
    <button type="button" className="chip" title="Copy session id" onClick={copy}>
      <span>{copied ? "Copied" : "Session"}</span>
      <code>{shortId(sessionId)}</code>
    </button>
  );
}

// The pane's last output, as a terminal: collapsed to its newest lines, with
// the rest one click away.
export function Terminal({
  tail,
  sessionId,
  actions,
}: {
  tail: string;
  sessionId: string | null;
  // The pane's controls: Open in terminal and View live.
  actions: React.ReactNode;
}) {
  const text = tidy(tail);
  const lines = lineCount(text);
  const body = useRef<HTMLPreElement>(null);
  const [open, setOpen] = useState(false);
  const [folds, setFolds] = useState(false);

  // The newest lines are the ones that matter: keep them in view while folded.
  const pin = useCallback(() => {
    const element = body.current;
    if (!element || open) return;
    setFolds((was) => was || element.scrollHeight > element.clientHeight + 1);
    element.scrollTop = element.scrollHeight;
  }, [open]);
  useLayoutEffect(pin, [pin, text]);
  useEffect(() => {
    const element = body.current;
    if (!element || typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(pin);
    observer.observe(element);
    return () => observer.disconnect();
  }, [pin]);

  if (lines === 0) return null;
  return (
    <div className="ev" data-open={open} data-testid="pane-tail">
      <div className="ev-head">
        <span className="chip">Pane tail</span>
        {sessionId ? <CopyChip sessionId={sessionId} /> : null}
        <span className="sp" />
        {actions}
      </div>
      <div className="term-wrap">
        <pre className="term" ref={body} tabIndex={0} aria-label="Pane tail">
          {parseAnsi(text).map((segment, index) => (
            <span
              key={index}
              className={[segment.bold && "b", segment.dim && "d", segment.italic && "i"]
                .filter(Boolean)
                .join(" ")}
              style={segment.fg ? { color: `rgb(${segment.fg})` } : undefined}
            >
              {segment.text}
            </span>
          ))}
        </pre>
        {folds && !open ? <div className="term-fade" /> : null}
      </div>
      {folds ? (
        <div className="ev-foot">
          <button
            type="button"
            aria-expanded={open}
            onClick={() => {
              setOpen(!open);
              if (open && body.current) body.current.scrollTop = body.current.scrollHeight;
            }}
          >
            {open ? "Show less" : `Show all ${lines} lines`}
          </button>
        </div>
      ) : null}
    </div>
  );
}
