import { call, callEither, callPlain, callEitherPlain } from './call';

export async function latestName(): Promise<string> {
  const out = await call('/v1/replies/latest');
  return out.json.data.name;
}

export async function firstOk(): Promise<boolean> {
  const out = await callEither('/v1/replies/first', true);
  return out.res.ok;
}

export async function plainName(): Promise<string> {
  const out = await callPlain('/v1/replies/latest');
  return out.json.data.name;
}

export async function plainFirstOk(): Promise<boolean> {
  const out = await callEitherPlain('/v1/replies/first', true);
  return out.res.ok;
}
