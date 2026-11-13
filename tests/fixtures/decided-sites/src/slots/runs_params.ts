import { useSend, plain } from "cache-kit";

export function sendOne() {
  return useSend({ send: (u) => fetch(u) });
}

export function readWithContext() {
  return useSend({ send: (ctx) => fetch(ctx.url) });
}

export function handOver() {
  return plain({ send: (w) => fetch(w) });
}
