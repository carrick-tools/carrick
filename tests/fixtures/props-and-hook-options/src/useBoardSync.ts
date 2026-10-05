import { useCallback } from "a-ui-runtime";

function withCache(init: RequestInit): RequestInit {
  const copy = { ...init };
  copy.cache = "no-store";
  return copy;
}

export function useBoardSync(syncUrl: string, init: RequestInit) {
  const sync = useCallback(() => fetch(syncUrl, withCache(init)), [syncUrl]);
  const peek = useCallback(() => fetch(syncUrl, init), [syncUrl]);
  return { sync, peek };
}
