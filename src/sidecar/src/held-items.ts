/**
 * A held service's `infer` answers, kept item by item (carrick#2881).
 *
 * After an edit, a daemon answers an `infer` request by reading again only
 * the items an edit can reach (`affected-set.ts`) and taking every other
 * item's answer from what it kept. The answer must equal a cold process's
 * byte for byte, so three things decide whether a kept item answers:
 *
 * - **Its key.** Everything the request says that can change the item's
 *   answer: the item itself and every request field beside the items
 *   (extraction config, model symbols, server claims).
 * - **Its file.** An edit that reaches the item's file drops it.
 * - **The items it was read beside.** The unwidened reading
 *   (`unwidened.ts`) rewrites every item's path at once and checks the
 *   rewritten files together, so a reading can depend on which other items
 *   share the batch. Two items are coupled when one's path holds a file the
 *   other's types read; a kept reading answers only when the items coupled to
 *   it now are the ones coupled to it when it was read, and none is read again.
 *   An item with no reading to make reads nothing of the others.
 */

import { requestDigest } from './infer-cache.js';
import type { InferItemOutcome, InferRequestItem } from './types.js';

/** One kept item. */
export interface HeldItem {
  /** The item's file, as the program names it. */
  file: string;
  outcome: InferItemOutcome;
  /** The keys of the items coupled to it when it was read, itself included, sorted; empty when none. */
  coupled: string[];
  /** Every file its types read when it was read: kept only for an item with a reading. */
  reads?: Set<string>;
}

/** Each item's key: the item, with every field of the request beside the items. */
export function itemKeysOf(request: { requests: InferRequestItem[] } & Record<string, unknown>): string[] {
  const { request_id: _id, requests, ...beside } = request;
  const context = requestDigest(beside);
  return requests.map((item) => requestDigest({ context, item }));
}

/** An item as the coupling reads it. */
export interface Coupling {
  key: string;
  pathFiles?: readonly string[];
  reads?: ReadonlySet<string>;
}

/**
 * For each item with a reading, the sorted keys of the items coupled to it
 * (transitively, itself included). Items with none are left out.
 */
export function couplingOf(items: readonly Coupling[]): Map<string, string[]> {
  const reading = items.filter((item) => (item.pathFiles?.length ?? 0) > 0);
  const parent = new Map<string, string>(reading.map((item) => [item.key, item.key]));
  const find = (key: string): string => {
    let root = key;
    while (parent.get(root) !== root) root = parent.get(root)!;
    parent.set(key, root);
    return root;
  };
  const union = (a: string, b: string): void => {
    const ra = find(a);
    const rb = find(b);
    if (ra !== rb) parent.set(ra, rb);
  };
  const byPathFile = new Map<string, string[]>();
  for (const item of reading) {
    for (const file of item.pathFiles!) byPathFile.set(file, [...(byPathFile.get(file) ?? []), item.key]);
  }
  for (const item of reading) {
    // Another item's rewrite reaches this one through a file its types read,
    // its own path files among them.
    for (const file of [...(item.reads ?? []), ...item.pathFiles!]) {
      for (const other of byPathFile.get(file) ?? []) union(item.key, other);
    }
  }
  const members = new Map<string, string[]>();
  for (const item of reading) {
    const root = find(item.key);
    members.set(root, [...(members.get(root) ?? []), item.key]);
  }
  const coupled = new Map<string, string[]>();
  for (const item of reading) coupled.set(item.key, [...new Set(members.get(find(item.key))!)].sort());
  return coupled;
}

/** Whether a kept reading still answers beside the items coupled to it now. */
export function sameCoupling(kept: readonly string[], now: readonly string[], readAgain: ReadonlySet<string>): boolean {
  if (kept.length !== now.length || kept.some((key, i) => key !== now[i])) return false;
  return !now.some((key) => readAgain.has(key));
}

/**
 * Drop the kept items an edit reached: every item in a file of `files`, and,
 * when an export was added or removed (which can move where an import
 * resolves, and so what a file reads), every item with a reading.
 */
export function dropReached(
  items: Map<string, HeldItem>,
  reached: { files: ReadonlySet<string>; exportsMoved: boolean } | 'all'
): number {
  const before = items.size;
  if (reached === 'all') {
    items.clear();
    return before;
  }
  for (const [key, item] of items) {
    if (reached.files.has(item.file) || (reached.exportsMoved && item.reads)) items.delete(key);
  }
  return before - items.size;
}
