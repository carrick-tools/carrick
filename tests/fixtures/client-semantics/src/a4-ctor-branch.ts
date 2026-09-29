import http from "@fixture/http";

export class Gateway {
  private api;
  constructor(beta: boolean) {
    this.api = http.create({ baseURL: "/stable" });
    if (beta) {
      this.api = http.create({ baseURL: "/beta" });
    }
  }
  list(): unknown {
    return this.api.get("/items");
  }
}
