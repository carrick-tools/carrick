import Steel from "steel-sdk";

export class Runner {
  private client: Steel;

  constructor() {
    this.client = new Steel({ apiKey: process.env.STEEL_API_KEY ?? "" });
  }

  async go(id: string): Promise<void> {
    await this.client.sessions.release(id);
  }
}
