// Calls shaped like requests through the client's verbs that are not the
// client. None of them is a consumer row.
import http from "@fixture/http";

const cache = new Map<string, string>();

export function cachedRoute(): string | undefined {
  return new Map<string, string>().get("/r");
}

export function lookup(): string | undefined {
  return cache.get("/r");
}

export function shadowed(): string | undefined {
  const http = new Map<string, string>();
  return http.get("/shadow");
}
