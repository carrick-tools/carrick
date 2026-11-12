export async function queryChoiceToString(q?: string): Promise<Response> {
  const url = q ? `/api/s1?query=${encodeURIComponent(q)}` : "/api/s1";
  return fetch(url.toString());
}

export async function pathChoiceToString(flag: boolean): Promise<Response> {
  const url = flag ? "/api/s2/one" : "/api/s2/two";
  return fetch(url.toString());
}

export async function inlineChoiceToString(flag: boolean): Promise<Response> {
  return fetch((flag ? "/api/s3/one" : "/api/s3/two").toString());
}

export async function plainToString(id: string): Promise<Response> {
  const url = `/api/s4/${id}`;
  return fetch(url.toString());
}

export async function queryChoiceNoToString(q?: string): Promise<Response> {
  const url = q ? `/api/s5?query=${encodeURIComponent(q)}` : "/api/s5";
  return fetch(url);
}
