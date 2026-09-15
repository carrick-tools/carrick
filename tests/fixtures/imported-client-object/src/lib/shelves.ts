import { sendJson } from "./http";

/**
 * The kinds of shelf a store can hold. A kind decides which listing a shelf
 * appears in and which fields its detail view shows; it never changes after
 * the shelf is created.
 */
export type ShelfKind = "ambient" | "chilled" | "frozen" | "display";

/**
 * A shelf as the listing returns it. Counts are computed at read time and are
 * not stored, so two reads a second apart can disagree while stock moves.
 */
export interface ShelfSummary {
  id: string;
  label: string;
  kind: ShelfKind;
  aisle: number;
  bay: number;
  itemCount: number;
  lowStockCount: number;
  lastCountedAt: string | null;
}

/**
 * A shelf with its slots. A slot holds at most one product line; an empty
 * slot has a null product and is still returned so the planogram renders.
 */
export interface ShelfDetail extends ShelfSummary {
  slots: ShelfSlot[];
  notes: string;
  createdAt: string;
  updatedAt: string;
}

/**
 * One slot on a shelf. Facings is how many units sit at the front edge; depth
 * is how many sit behind each facing. Capacity is their product.
 */
export interface ShelfSlot {
  position: number;
  productId: string | null;
  facings: number;
  depth: number;
  onHand: number;
  reorderPoint: number;
}

/**
 * The payload a relabel sends. Labels are trimmed and collapsed server-side,
 * so the value echoed back can differ from the one sent.
 */
export interface RelabelRequest {
  label: string;
}

/**
 * What archiving returns. An archived shelf keeps its history and its slots,
 * and stops appearing in listings unless the caller asks for archived shelves.
 */
export interface ArchiveResult {
  id: string;
  archivedAt: string;
  movedItems: number;
}

/**
 * A count session: one pass over a shelf by one person. Sessions that are
 * never closed expire after a working day and their partial counts are kept.
 */
export interface CountSession {
  id: string;
  shelfId: string;
  startedBy: string;
  startedAt: string;
  closedAt: string | null;
  lines: CountLine[];
}

/**
 * One counted slot. A variance is the counted quantity less the expected one,
 * and is only recorded when the two differ.
 */
export interface CountLine {
  position: number;
  expected: number;
  counted: number;
  variance: number | null;
}

/**
 * Filters the listing accepts. Every field is optional; an empty filter lists
 * every live shelf in aisle order.
 */
export interface ShelfFilter {
  kind?: ShelfKind;
  aisle?: number;
  lowStockOnly?: boolean;
  includeArchived?: boolean;
}

/**
 * A planned change to a shelf's layout. Plans are drafted against the current
 * slots and applied in one step overnight; a plan whose base layout changed
 * since it was drafted is rejected and has to be redrafted.
 */
export interface LayoutPlan {
  id: string;
  shelfId: string;
  draftedBy: string;
  draftedAt: string;
  baseRevision: number;
  moves: SlotMove[];
  status: "draft" | "scheduled" | "applied" | "rejected";
}

/**
 * One move within a layout plan. A move with no target position removes the
 * product line from the shelf; one with no source position introduces it.
 */
export interface SlotMove {
  productId: string;
  fromPosition: number | null;
  toPosition: number | null;
  facings: number;
}

/**
 * Stock levels at which a shelf is flagged. Thresholds are per kind, because a
 * frozen bay empties on a different clock from an ambient one.
 */
export interface ReplenishmentThresholds {
  kind: ShelfKind;
  lowStockRatio: number;
  criticalRatio: number;
  recountAfterDays: number;
}

/**
 * Human-readable label for a shelf, shown in breadcrumbs and in the count
 * screen header. Kept here so every screen spells it the same way.
 */
export function formatShelfLabel(label: string): string {
  return label.trim().replace(/\s+/g, " ");
}

/**
 * Sort shelves the way a walk through the store meets them: by aisle, then by
 * bay, then by label so equal positions still order stably.
 */
export function walkOrder(a: ShelfSummary, b: ShelfSummary): number {
  if (a.aisle !== b.aisle) return a.aisle - b.aisle;
  if (a.bay !== b.bay) return a.bay - b.bay;
  return a.label.localeCompare(b.label);
}

export const shelves = {
  list: () => sendJson<ShelfSummary[]>("GET", "/v2/shelves"),

  get: (shelfId: string) => sendJson<ShelfDetail>("GET", `/v2/shelves/${shelfId}`),

  rename: (shelfId: string, label: string) =>
    sendJson<ShelfDetail>("PATCH", `/v2/shelves/${shelfId}/label`, { label } satisfies RelabelRequest),

  archive: (shelfId: string) => sendJson<ArchiveResult>("POST", `/v2/shelves/${shelfId}/archive`),
};
