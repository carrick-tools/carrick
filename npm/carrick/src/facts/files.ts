// The files one `factLines` call reads, each read and scanned at most once
// (plan section 33, R33.5): the edited file, the files a name line points at,
// the peer files an op or fn line's proof hashes, the manifests a pkg line
// checks. Every read is capped at `MAX_FACT_FILE_BYTES`; a larger file, a
// missing one or an unreadable one is null, and the rule asking says nothing.

import fs from "node:fs";
import { scanSource } from "./match.ts";
import type { Scan } from "./match.ts";

/** A file larger than this gets no fact lines (and is not read). */
export const MAX_FACT_FILE_BYTES = 512 * 1024;

/** The files the declaration scanner reads. */
export const SCANNED = /\.(ts|tsx|mts|cts|js|jsx|mjs|cjs)$/;

export function isJsx(file: string): boolean {
  return /\.(tsx|jsx)$/.test(file);
}

/** A file's text and its clean declaration scan. */
export type ScannedFile = { text: string; scan: Scan };

export type FileMemo = {
  /** The file's bytes, or null when missing, unreadable or over `MAX_FACT_FILE_BYTES`. */
  bytes: (file: string) => Buffer | null;
  /** The file's text (UTF-8), with the same nulls as `bytes`. */
  text: (file: string) => string | null;
  /** The file's text and declaration scan, or null when it is not a file the scanner reads, has no text, or does not scan cleanly. */
  scanned: (file: string) => ScannedFile | null;
};

/** A fresh memo, for one `factLines` call. */
export function fileMemo(): FileMemo {
  const bytes = new Map<string, Buffer | null>();
  const texts = new Map<string, string | null>();
  const scans = new Map<string, ScannedFile | null>();
  const readBytes = (file: string): Buffer | null => {
    if (bytes.has(file)) return bytes.get(file)!;
    let out: Buffer | null = null;
    try {
      if (fs.statSync(file).size <= MAX_FACT_FILE_BYTES) out = fs.readFileSync(file);
    } catch {
      out = null;
    }
    bytes.set(file, out);
    return out;
  };
  const readText = (file: string): string | null => {
    if (texts.has(file)) return texts.get(file)!;
    const out = readBytes(file)?.toString("utf8") ?? null;
    texts.set(file, out);
    return out;
  };
  return {
    bytes: readBytes,
    text: readText,
    scanned: (file) => {
      if (scans.has(file)) return scans.get(file)!;
      let out: ScannedFile | null = null;
      const text = SCANNED.test(file) ? readText(file) : null;
      if (text !== null) {
        const scan = scanSource(text, isJsx(file));
        out = scan.ok ? { text, scan } : null;
      }
      scans.set(file, out);
      return out;
    },
  };
}
