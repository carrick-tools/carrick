// The request helper the client's member calls. Its type argument defaults to
// `any`, so a call that states none gets a response with no type of its own:
// the judge leaves the pair to the consumer's reads (the retype check).

export type RequestOptions = {
  method: string;
};

export async function send<T = any>(url: string, options: RequestOptions): Promise<T> {
  const response = await fetch(url, options);
  return (await response.json()) as T;
}
