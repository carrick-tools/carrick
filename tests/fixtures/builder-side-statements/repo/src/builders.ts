// Builders that put a query on a path. A call on the query local changes only
// the query, which is never part of the route, so the path stays the route.

// The query is set only when there is a filter.
export function guardedUrl(id: string, key: string, filter?: string) {
  const query = new URLSearchParams();
  if (filter) {
    query.set("filter", filter);
  }
  return `/api/b/${id}/guarded/${key}?${query}`;
}

// The query is always set.
export function plainUrl(key: string) {
  const query = new URLSearchParams();
  query.set("v", "1");
  return `/api/b/plain/${key}?${query.toString()}`;
}

// A call on something that is not a query local may change the path.
export function loggedUrl(key: string) {
  const path = `/api/b/logged/${key}`;
  console.log(path);
  return path;
}

// The local the call changes is a segment of the path.
export function segmentUrl(key: string) {
  const parts = ["api", "b", "segment"];
  parts.push(key);
  return `/${parts.join("/")}`;
}
