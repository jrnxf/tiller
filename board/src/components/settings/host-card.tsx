import type { ReactNode } from "react";

import type { Tone, Words } from "./check-lines";

// A host's card: what opens under its row in Settings > Hosts. Each block of
// it is a HostCardSection, the checks first; a host's own settings and its
// pairing add their sections below in the same shape.
export function HostCard({
  id,
  host,
  children,
}: {
  id: string;
  host: string;
  children: ReactNode;
}) {
  return (
    <div className="set-card" id={id} data-testid={`host-card-${host}`}>
      {children}
    </div>
  );
}

// One block of the card: a title, a quiet fact beside it, and its action on
// the right of the head.
export function HostCardSection({
  title,
  meta,
  action,
  children,
}: {
  title: string;
  meta?: ReactNode;
  action?: ReactNode;
  children: ReactNode;
}) {
  return (
    <section className="set-card-sec" aria-label={title}>
      <div className="set-card-h">
        <h3>{title}</h3>
        {meta ? <span className="set-card-m">{meta}</span> : null}
        {action ? <span className="set-card-a">{action}</span> : null}
      </div>
      {children}
    </section>
  );
}

function WordsText({ words }: { words: Words }) {
  return (
    <>
      {words.map((word, index) =>
        typeof word === "string" ? word : <code key={index}>{word.code}</code>,
      )}
    </>
  );
}

// One line of a card's list: a dot, what it is about, what it says, and on
// the right either a quiet note or the line's fix.
export function CardLine({
  tone,
  title,
  sub,
  detail,
  end,
  testid,
}: {
  tone: Tone | null;
  title: string;
  sub?: string;
  detail?: Words;
  end?: ReactNode;
  testid?: string;
}) {
  return (
    <li data-testid={testid} data-tone={tone ?? undefined}>
      <span className={`set-dot${tone ? ` ${tone}` : ""}`} aria-hidden="true" />
      <span className="set-line-t">
        {title}
        {sub ? <small>{sub}</small> : null}
      </span>
      {detail && detail.length > 0 ? (
        <span className="set-line-d">
          <WordsText words={detail} />
        </span>
      ) : null}
      {end}
    </li>
  );
}
