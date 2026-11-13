// A call line states one operation however many texts the code writes for
// it. Each function below is one shape; rows.expected.json is the key.

// Same route, once with a query: one row.
export async function sameRouteWithQuery(q?: string): Promise<Response> {
  let url = "/api/same";
  if (q) url = "/api/same?query=1";
  return fetch(url);
}

// Two routes: two rows.
export async function twoRoutes(q?: string): Promise<Response> {
  let url = "/api/first";
  if (q) url = "/api/second";
  return fetch(url);
}

// A constant conditional, route with and without a query: one row.
export async function constantConditional(q?: string): Promise<Response> {
  const url = q ? "/api/const?x=1" : "/api/const";
  return fetch(url);
}

// Two fetches of one route on one line: one row.
export async function twoFetchesOneLine(): Promise<void> {
  await fetch("/api/twice"); await fetch("/api/twice");
}

// Two different queries on one route: one row.
export async function twoQueries(q?: string): Promise<Response> {
  let url = "/api/queries?a=1";
  if (q) url = "/api/queries?b=2";
  return fetch(url);
}

// An inline conditional over a route with and without a query: one row.
export async function inlineConditional(q?: string): Promise<Response> {
  return fetch(q ? "/api/inline?x=1" : "/api/inline");
}

// A base constant, rewritten with a fragment: one row.
const BASE = "/api";
export async function baseThenFragment(q?: string): Promise<Response> {
  let url = `${BASE}/based`;
  if (q) url = "/api/based#frag";
  return fetch(url);
}
