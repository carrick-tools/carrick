// A class in this file overrides the builder: on one of its instances the base
// class's own call reaches the override, which builds a path.
export class BaseClient {
  constructor(protected base: string) {}

  public async open(kind: string, ref: string | null) {
    return await this.submit({
      target: `${this.base}/refused/overridden/${kind}${this.query(ref)}`,
    });
  }

  protected async submit({ target }: { target: string }) {
    const res = await fetch(target, { method: "POST" });
    return res.json();
  }

  protected query(ref: string | null): string {
    return ref ? `?ref=${ref}` : "";
  }
}

export class PathClient extends BaseClient {
  protected query(ref: string | null): string {
    return ref ? `/${ref}` : "";
  }
}

// The class replaces the builder it declares.
export class RewiredClient {
  private base: string;

  constructor(host: string, build: (ref: string | null) => string) {
    this.base = host;
    this.query = build;
  }

  public async open(kind: string, ref: string | null) {
    return await this.submit({
      target: `${this.base}/refused/rewired/${kind}${this.query(ref)}`,
    });
  }

  private async submit({ target }: { target: string }) {
    const res = await fetch(target, { method: "POST" });
    return res.json();
  }

  private query(ref: string | null): string {
    return ref ? `?ref=${ref}` : "";
  }
}
