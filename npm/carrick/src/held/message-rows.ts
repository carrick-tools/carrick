// The socket send form's join (HM-2, unwired): an emitter row has no position
// of its own; the listener row's places carry the emit's payload span. For each
// emitter (consumer) row, find the listener (producer) rows' places at the same
// repo, file, line and label with a span, and hand the emitter back with
// `values` set to them. Pure: nothing here reads a store or arms a check.

import type { ContractRow } from "../facts/store.ts";

/** The emitter rows of `rows`, each with `values` from the listener rows' places; none when no place has a span. */
export function messageSendRows(rows: ContractRow[]): ContractRow[] {
  const listeners = rows.filter((r) => r.side === "producer");
  const out: ContractRow[] = [];
  for (const row of rows) {
    if (row.side !== "consumer") continue;
    const seen = new Set<string>();
    const values: Array<{ file: string; span: [number, number] }> = [];
    for (const listener of listeners) {
      if (listener.label !== row.label) continue;
      for (const group of listener.groups) {
        for (const place of group.places) {
          if (place.repo !== row.repo || place.file !== row.file || place.line !== row.line) continue;
          if (!place.span) continue;
          const key = `${place.file}\0${place.span[0]}\0${place.span[1]}`;
          if (seen.has(key)) continue;
          seen.add(key);
          values.push({ file: place.file, span: [place.span[0], place.span[1]] });
        }
      }
    }
    if (values.length === 0) {
      out.push(row);
      continue;
    }
    values.sort((a, b) => (a.file < b.file ? -1 : a.file > b.file ? 1 : a.span[0] - b.span[0]));
    out.push({ ...row, values });
  }
  return out;
}
