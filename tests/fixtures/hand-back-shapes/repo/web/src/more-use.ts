import { viaPack, viaNew, viaSpread, viaClone, viaSend, viaDeclared, viaTuple, viaHeaders } from './more';

export async function b1() {
  const out = await viaPack('/v1/replies/latest');
  return out;
}

export async function b2() {
  const out = await viaNew('/v1/replies/latest');
  return out;
}

export async function b3() {
  const out = await viaSpread('/v1/replies/latest');
  return out;
}

export async function b4() {
  const out = await viaClone('/v1/replies/latest');
  return out;
}

export async function b5() {
  const out = await viaSend('/v1/replies/latest');
  return out;
}


export async function b7() {
  const out = await viaDeclared('/v1/replies/latest');
  return out;
}

export async function b8() {
  const out = await viaTuple('/v1/replies/latest');
  return out;
}

export async function b9() {
  const out = await viaHeaders('/v1/replies/latest');
  return out;
}
