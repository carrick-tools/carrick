// Builders that put a query on a path. A call on the query local changes only
// the query, which is never part of the route, so the path stays the route.

// The query is set only when there is a filter.
function guardedUrl(id: string, key: string, filter?: string) {
  const query = new URLSearchParams();
  if (filter) {
    query.set("filter", filter);
  }
  return `/api/b/${id}/guarded/${key}?${query}`;
}

// The query is always set.
function plainUrl(key: string) {
  const query = new URLSearchParams();
  query.set("v", "1");
  return `/api/b/plain/${key}?${query.toString()}`;
}

// A call on something that is not a query local may change the path.
function loggedUrl(key: string) {
  const path = `/api/b/logged/${key}`;
  console.log(path);
  return path;
}

// The local the call changes is a segment of the path.
function segmentUrl(key: string) {
  const parts = ["api", "b", "segment"];
  parts.push(key);
  return `/${parts.join("/")}`;
}

export async function load(id: string, key: string) {
  await fetch(guardedUrl(id, key), { method: "GET" });
  await fetch(plainUrl(key), { method: "GET" });
  await fetch(loggedUrl(key), { method: "GET" });
  await fetch(segmentUrl(key), { method: "GET" });
}
