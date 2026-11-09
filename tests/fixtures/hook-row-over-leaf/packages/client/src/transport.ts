export interface RequestSpec {
  url: string;
  path?: Record<string, string>;
  body?: unknown;
}

export interface Transport {
  get(spec: RequestSpec): Promise<{ data: unknown }>;
  put(spec: RequestSpec): Promise<{ data: unknown }>;
  post(spec: RequestSpec): Promise<{ data: unknown }>;
}

export interface RequestOptions {
  client?: Transport;
  path?: Record<string, string>;
  body?: unknown;
}

function fill(spec: RequestSpec): string {
  return Object.entries(spec.path ?? {}).reduce(
    (url, [name, value]) => url.replace(`{${name}}`, value),
    spec.url,
  );
}

async function exchange(verb: string, spec: RequestSpec) {
  const response = await fetch(fill(spec), {
    method: verb,
    body: spec.body === undefined ? undefined : JSON.stringify(spec.body),
  });
  return { data: await response.json() };
}

export const transport: Transport = {
  get: (spec) => exchange("GET", spec),
  put: (spec) => exchange("PUT", spec),
  post: (spec) => exchange("POST", spec),
};
