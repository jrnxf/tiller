import type { BoardFilter, BoardResponse, BoardRow } from "../../lib/api-types";
import { ownerKey } from "../../lib/peers";

// The five columns of the board, left to right as the work progresses. `asked`
// is not a card column: it holds the unresolved asks (state.md, section 6).
export type ColumnId = "asked" | "queued" | "working" | "landing" | "done";

export const COLUMNS: { id: ColumnId; name: string }[] = [
  { id: "asked", name: "Asked" },
  { id: "queued", name: "Queued" },
  { id: "working", name: "Working" },
  { id: "landing", name: "Landing" },
  { id: "done", name: "Done" },
];

// A card tile on the board: a card row, or an ask that is not a card yet.
export interface Tile {
  key: string;
  column: ColumnId;
  projectId: string;
  hostId: string | null;
  // Set for cards; asks have no card page.
  row: BoardRow | null;
  // Set for asks.
  ask: { id: string; age_seconds: number; received_at: string } | null;
}

export function tilesOf(board: BoardResponse): Tile[] {
  const cards = board.rows.map<Tile>((row) => ({
    key: row.id,
    column: row.column,
    projectId: ownerKey(row.project_id, row),
    hostId: row.host_id,
    row,
    ask: null,
  }));
  const asks = board.asked.map<Tile>((ask) => ({
    key: ask.id,
    column: "asked",
    projectId: ask.project_id,
    hostId: null,
    row: null,
    ask,
  }));
  return [...asks, ...cards];
}

export function passesFilter(tile: Tile, filter: BoardFilter): boolean {
  if (filter === "all") return true;
  if (filter === "active") return tile.column !== "done";
  return tile.row?.needs_you === true;
}

export function countsByFilter(tiles: Tile[]): Record<BoardFilter, number> {
  return {
    all: tiles.length,
    active: tiles.filter((tile) => passesFilter(tile, "active")).length,
    needs: tiles.filter((tile) => passesFilter(tile, "needs")).length,
  };
}

// The tiles the board shows for a scope: the lanes the view lets through
// (`lanes` holds their project keys) and the machine filter. The header's
// All, Active and Needs you counts and the sidebar's Board count all come
// from this one list, so they cannot disagree.
export function tilesInScope(
  board: BoardResponse,
  lanes: Set<string>,
  host: string | undefined,
): Tile[] {
  return tilesOf(board).filter((tile) => lanes.has(tile.projectId) && onHost(tile, host));
}

// What a machine filter keeps: cards that run there. An ask has no machine, so
// it stays out of a filtered view, like the lane-less cards.
export function onHost(tile: Tile, host: string | undefined): boolean {
  return !host || tile.hostId === host;
}

export function parseFilter(value: unknown): BoardFilter {
  return value === "active" || value === "needs" ? value : "all";
}

// The number the lane header shows: open cards, not done and not asks.
export function openCount(tiles: Tile[]): number {
  return tiles.filter((tile) => tile.column !== "done" && tile.row !== null).length;
}
