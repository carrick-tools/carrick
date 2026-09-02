// Four outbound requests from one client. Three of them build their target as
// `new URL(path, base)`: the path literal is the first argument, and the base
// is opaque, as it usually is in real code. It might be a field set from
// configuration, a parameter, an env value. The path is the only thing the
// source states about the route.
export class CatalogueClient {
  constructor(private readonly baseUrl: string) {}

  // The decoy. A neighbouring request on the retired version, written inline,
  // which is what makes `/api/v1/...` a plausible thing for extraction to reach
  // for on the three sites below.
  async createToken(): Promise<Response> {
    return fetch(`${this.baseUrl}/api/v1/token`, { method: "POST" });
  }

  // Direct form: the URL object is the request's own argument.
  async listThings(): Promise<Response> {
    return fetch(new URL("/api/v2/things", this.baseUrl), { method: "GET" });
  }

  // Binding form: a local const read back through `.href`, which is how a URL
  // object is nearly always handed to a request, because the search params get
  // appended in between.
  async findThings(query: string): Promise<Response> {
    const url = new URL("/api/v2/things/search", this.baseUrl);
    url.searchParams.append("q", query);
    return fetch(url.href, { method: "GET" });
  }

  // Template form: a literal head with an interpolated segment behind it.
  async archiveThing(id: string): Promise<Response> {
    const url = new URL(`/api/v2/things/${id}/archive`, this.baseUrl);
    return fetch(url, { method: "POST" });
  }
}
