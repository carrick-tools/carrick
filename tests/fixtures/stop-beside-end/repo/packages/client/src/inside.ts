import { Port } from "./mixed";

const ports: Record<string, Port> = {};

export async function keyedBoth(name: string): Promise<void> {
  await fetch("/v1/api/keyed");
  await ports[name].send("k", {});
}

export async function keyedOnly(name: string): Promise<void> {
  await ports[name].send("k", {});
}

export class FieldRelay {
  constructor(private readonly port: Port) {}
  async fieldBoth(): Promise<void> {
    await fetch("/v1/api/field");
    await this.port.send("f", {});
  }
  async fieldOnly(): Promise<void> {
    await this.port.send("f", {});
  }
}
