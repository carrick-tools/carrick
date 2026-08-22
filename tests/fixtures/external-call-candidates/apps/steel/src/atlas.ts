import type { Session } from "atlas-scraper";
import Steel from "steel-sdk";

// The vendor's shape, declared here rather than imported.
interface Scraper {
  scrape(url: string): Promise<string>;
}

const steel = new Steel({ apiKey: "" });

// A parameter typed by an interface this file declares names no package.
export const capture = (local: Scraper, url: string): Promise<string> =>
  local.scrape(url);

// A parameter typed by one it imports from a dependency names a package it
// still must not read: an annotation on a parameter says what the caller is
// expected to pass, not what the caller passed.
export const captureVendor = (vendor: Session, url: string): Promise<string> =>
  vendor.scrape(url);

// A callback's parameter names nothing either, even when the promise it
// settles came out of a receiver that does resolve. The outer call is the
// row; the call inside the callback is not.
export const captureLater = (url: string): Promise<string> =>
  steel.sessions.open(url).then((session) => session.scrape(url));
