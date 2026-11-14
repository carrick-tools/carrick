import { retried, envelope } from './lib';



export async function c3() {
  const out = await retried('/v1/replies/latest');
  return out;
}

export async function c4() {
  const out = await envelope('/v1/replies/latest');
  return out;
}
