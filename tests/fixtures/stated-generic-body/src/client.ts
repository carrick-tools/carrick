import type { Envelope, Listing, Member, Order, OrderEnvelope } from "./shapes";

// An instantiation behind `Promise`, cast at the read.
export async function listOrders() {
  const res = await fetch("/api/orders");
  return res.json() as Promise<Envelope<Order[]>>;
}

// An instantiation cast after the read is awaited.
export async function getMember(id: string) {
  const res = await fetch(`/api/members/${id}`);
  return (await res.json()) as Envelope<Member>;
}

// An instantiation annotated on the declaration the read initializes.
export async function listMembers() {
  const res = await fetch("/api/members");
  const page: Envelope<Member[]> = await res.json();
  return page;
}

// An alias of an instantiation: no type argument is written at the read.
export async function latestOrders() {
  const res = await fetch("/api/orders/latest");
  return res.json() as Promise<OrderEnvelope>;
}

// A generic with a default argument, written bare.
export async function openOrders() {
  const res = await fetch("/api/orders/open");
  return (await res.json()) as Listing;
}

// The same generic, written with an argument.
export async function activeMembers() {
  const res = await fetch("/api/members/active");
  return (await res.json()) as Listing<Member>;
}

// A union of two instantiations.
export async function search(term: string) {
  const res = await fetch(`/api/search/${term}`);
  return (await res.json()) as Envelope<Order[]> | Envelope<Member[]>;
}

// An instantiation that may be absent.
export async function findOrder(id: string) {
  const res = await fetch(`/api/orders/${id}`);
  return (await res.json()) as Envelope<Order> | null;
}

// A generic the repo does not declare, over a type it does.
export async function ordersById() {
  const res = await fetch("/api/orders/by-id");
  return (await res.json()) as Record<string, Order>;
}
