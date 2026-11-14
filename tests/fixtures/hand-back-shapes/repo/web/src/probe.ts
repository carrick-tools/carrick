import { probeFetchWhole, probeResWhole, probeNested, probeChain, probeNoTransport, probeBody, probeTuple } from './call';

export async function a1(): Promise<boolean> {
  const out = await probeFetchWhole('/v1/replies/latest');
  return out.ok;
}

export async function a2(): Promise<boolean> {
  const out = await probeResWhole('/v1/replies/latest');
  return out.ok;
}

export async function a3(): Promise<string> {
  const out = await probeNested('/v1/replies/latest');
  return out.json.data.name;
}


export async function a5(): Promise<string> {
  const out = await probeChain('/v1/replies/latest');
  return out.json.data.name;
}

export async function a6(): Promise<string> {
  const out = await probeNoTransport('/v1/replies/latest');
  return out.json.data.name;
}

export async function a7(): Promise<string> {
  const out = await probeBody('/v1/replies/latest');
  return out.name;
}

export async function a8(): Promise<boolean> {
  const out = await probeTuple('/v1/replies/latest');
  return out[0].ok;
}
