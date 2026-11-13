export async function appendQueryInBranch(q?: string): Promise<Response> {
  let url = "/api/notes";
  if (q) {
    url += `?term=${encodeURIComponent(q)}`;
  }
  return fetch(url);
}

export async function appendSegmentAlways(): Promise<Response> {
  let url = "/api/items";
  url += "/all";
  return fetch(url);
}

export async function appendTwice(q?: string): Promise<Response> {
  let url = "/api/twice";
  url += "/a";
  if (q) url += "/b";
  return fetch(url);
}

export async function rewriteThenAppend(flag: boolean): Promise<Response> {
  let url = "/api/first";
  if (flag) url = "/api/second";
  url += "?x=1";
  return fetch(url);
}

export async function appendInLoop(parts: string[]): Promise<Response> {
  let url = "/api/loop";
  for (const part of parts) {
    url += "/" + part;
  }
  return fetch(url);
}

export async function appendInClosure(q?: string): Promise<Response> {
  let url = "/api/closure";
  const add = () => {
    url += "?q=1";
  };
  if (q) add();
  return fetch(url);
}

export async function selfConcat(q?: string): Promise<Response> {
  let url = "/api/self";
  if (q) url = url + "?q=1";
  return fetch(url);
}

export async function numberAppend(): Promise<Response> {
  let page = 1;
  page += 2;
  return fetch(`/api/pages/${page}`);
}

export async function appendName(suffix: string): Promise<Response> {
  let url = "/api/named";
  url += suffix;
  return fetch(url);
}

export async function appendInBothBranches(a: boolean): Promise<Response> {
  let url = "/api/both";
  if (a) url += "/x";
  else url += "/y";
  return fetch(url);
}
