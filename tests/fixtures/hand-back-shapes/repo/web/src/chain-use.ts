import { thenParse, thenHold, catchParse, retryFetch, handled } from './chain';

export async function d1() {
  const out = await thenParse('/v1/replies/latest');
  return out;
}

export async function d2() {
  const out = await thenHold('/v1/replies/latest');
  return out;
}

export async function d3() {
  const out = await catchParse('/v1/replies/latest');
  return out;
}

export async function d4() {
  const out = await retryFetch('/v1/replies/latest');
  return out;
}

export async function d5() {
  const out = await handled('/v1/replies/latest');
  return out;
}
