const send = (url: string): Promise<Response> => fetch(url);

// A function the caller hands in under the key `run`. The only caller writes
// it through a spread of a local binding, so the key reads `send` and the
// call below enters it.
export const call = (options: { run: (url: string) => Promise<Response>; signal?: AbortSignal }) => {
  return options.run('/api/v1/same');
};

export const reload = (signal: AbortSignal) => {
  const base = { run: send };
  return call({ ...base, signal });
};
