import Forge from "forge-sdk";

export class Runner {
  private client: Forge;

  constructor() {
    this.client = new Forge({ apiKey: process.env.FORGE_API_KEY ?? "" });
  }

  async go(id: string): Promise<void> {
    await this.client.sessions.release(id);
  }
}
