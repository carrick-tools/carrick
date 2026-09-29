import http from "@fixture/http";

export class BaseApi {
  protected api = http.create({ baseURL: "/default" });
  list(): unknown {
    return this.api.get("/items");
  }
}

export class UsersApi extends BaseApi {
  protected api = http.create({ baseURL: "/users-svc" });
}

export function block(): unknown {
  const api = http.create({ baseURL: "/outer" });
  {
    const api = new Map<string, string>();
    api.get("/inner-map");
  }
  return api.get("/outer-ok");
}
