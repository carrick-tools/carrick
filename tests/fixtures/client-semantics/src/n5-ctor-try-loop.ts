import http from "@fixture/http";

export class TryCtor {
  private api = http.create({ baseURL: "/try-init" });
  constructor() {
    try {
      this.api = http.create({ baseURL: "/try-body" });
    } catch {
      /* keep */
    }
  }
  list(): unknown { return this.api.get("/try"); }
}

export class LoopCtor {
  private api;
  constructor(bases: string[]) {
    this.api = http.create({ baseURL: "/loop-first" });
    for (const base of bases) {
      this.api = http.create({ baseURL: base });
    }
  }
  list(): unknown { return this.api.get("/loop"); }
}
