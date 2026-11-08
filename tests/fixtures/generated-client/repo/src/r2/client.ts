// Rung 2: members are made by a method factory over a closure `request`;
// the client type is a mapped type over the method names, and the object
// is asserted to it.
type Method = 'get' | 'post';
type Options = { url: string; body?: unknown };
type MethodFn = (options: Options) => Promise<Response>;
export type Client = { [K in Method]: MethodFn } & { request: (options: Options & { method: string }) => Promise<Response> };

export const createClient = (): Client => {
  const request = async (options: Options & { method: string }) =>
    fetch(options.url, { method: options.method, body: JSON.stringify(options.body) });

  const makeMethodFn = (method: string) => (options: Options) => request({ ...options, method });

  return {
    get: makeMethodFn('GET'),
    post: makeMethodFn('POST'),
    request,
  } as Client;
};

export const client = createClient();
