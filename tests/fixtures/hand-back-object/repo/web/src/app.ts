import { call, callEither } from './call';

export async function latestName(): Promise<string> {
  const out = await call('/v1/replies/latest');
  return out.json.data.name;
}

export async function firstOk(): Promise<boolean> {
  const out = await callEither('/v1/replies/first', true);
  return out.res.ok;
}
