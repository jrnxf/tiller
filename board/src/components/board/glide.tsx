import {
  ViewTransition,
  addTransitionType,
  startTransition,
  useEffect,
  useMemo,
  useState,
} from "react";
import type { ReactNode } from "react";

import type { BoardResponse, BoardRow } from "../../lib/api-types";
import { ownerOf } from "../../lib/peers";
import { tilesOf } from "./model";

// A card that moves glides to its new place with the browser's View
// Transitions. Only a board update the daemon sent carries this transition
// type: first paint and filter or tab changes render at once.
const CARD_MOVES = "card-moves";
const GLIDE = { [CARD_MOVES]: "card-glide", default: "none" };

// Where every tile sits, in board order: its lane and its column.
function placesOf(board: BoardResponse): Map<string, string> {
  return new Map(tilesOf(board).map((tile) => [tile.key, `${tile.projectId}/${tile.column}`]));
}

// Whether a card on both boards changed its lane, its column or its order. A
// card that only arrives or leaves moves nothing, so it needs no glide.
function movesBetween(from: Map<string, string>, to: Map<string, string>): boolean {
  const before = [...from].filter(([key]) => to.has(key));
  const after = [...to].filter(([key]) => from.has(key));
  return after.some(([key, place], index) => before[index]?.[0] !== key || from.get(key) !== place);
}

// The board the lanes draw. A read from the daemon that moves a card reaches
// it inside a transition, so React runs it as a View Transition and each card
// that moved glides there. Any other read shows at once: a View Transition
// holds the page still, so one for every read would eat clicks.
export function useMovingBoard(live: BoardResponse): BoardResponse {
  const [shown, setShown] = useState(live);
  const from = useMemo(() => placesOf(shown), [shown]);
  const to = useMemo(() => placesOf(live), [live]);
  const moved = movesBetween(from, to);
  if (!moved && shown !== live) setShown(live);
  useEffect(() => {
    if (!moved) return;
    startTransition(() => {
      addTransitionType(CARD_MOVES);
      setShown(live);
    });
  }, [moved, live]);
  return shown;
}

// Every character outside [a-zA-Z0-9] spelled as its code, so two different
// texts never spell the same.
function spell(text: string): string {
  return text.replace(/[^a-zA-Z0-9]/g, (char) => `_${char.charCodeAt(0).toString(16)}_`);
}

// One name per card on the page, so the browser pairs the card in its old
// column with the card in its new one. A peer's card carries its owner.
function glideName(row: BoardRow): string {
  return `card-${spell(ownerOf(row) ?? "")}-${spell(row.id)}`;
}

export function Glide({ row, children }: { row: BoardRow; children: ReactNode }) {
  return (
    <ViewTransition name={glideName(row)} default="none" share={GLIDE} update={GLIDE}>
      {children}
    </ViewTransition>
  );
}
