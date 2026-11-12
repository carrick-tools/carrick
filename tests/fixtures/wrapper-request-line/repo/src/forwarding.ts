import { forward } from './net/forward';

// Holds the request open and has no caller: its callers are not read.
export function forwardAny(endpoint: string) {
  return forward(endpoint);
}
