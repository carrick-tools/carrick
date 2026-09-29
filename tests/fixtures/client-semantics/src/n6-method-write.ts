import http from "@fixture/http";

export class Reconfig {
  private api = http.create({ baseURL: "/initial" });
  point(base: string): void {
    this.api = http.create({ baseURL: base });
  }
  list(): unknown { return this.api.get("/reconfig"); }
}

export class DefaultsWrite {
  private api = http.create({ baseURL: "/dw" });
  retarget(): void {
    this.api.defaults.baseURL = "/elsewhere";
  }
  list(): unknown { return this.api.get("/dw-list"); }
}
