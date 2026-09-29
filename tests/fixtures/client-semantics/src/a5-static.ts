import http from "@fixture/http";

export class Both {
  static api = http.create({ baseURL: "/static" });
  api = http.create({ baseURL: "/instance" });
  static load(): unknown {
    return this.api.get("/loaded");
  }
  run(): unknown {
    return this.api.get("/ran");
  }
}
