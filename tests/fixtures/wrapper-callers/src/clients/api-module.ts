// One module's request wrappers. `callApi` writes its URL into a local first;
// `callApiInline` writes the same URL in the call. The `api` and `inlineApi`
// members are a second hop: they pass the path on to the wrapper, and the page
// components call them.
declare const config: { SERVICE_ROOT_URL: string };

async function callApi(endpoint: string, init: RequestInit = {}): Promise<unknown> {
  const url = `${config.SERVICE_ROOT_URL}${endpoint}`;
  const res = await fetch(url, init);
  return res.json();
}

async function callApiInline(endpoint: string, init: RequestInit = {}): Promise<unknown> {
  const res = await fetch(`${config.SERVICE_ROOT_URL}${endpoint}`, init);
  return res.json();
}

export const api = {
  getEntities: () => callApi("/v1/widgets/list", { method: "GET" }),
  create: (body: string) => callApi("/v1/widgets/add", { method: "POST", body }),
};

export const inlineApi = {
  getEntities: () => callApiInline("/v1/widgets/list", { method: "GET" }),
  create: (body: string) => callApiInline("/v1/widgets/add", { method: "POST", body }),
};
