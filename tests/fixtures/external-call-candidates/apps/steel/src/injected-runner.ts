import type Steel from "steel-sdk";

export class InjectedRunner {
  private readonly client!: Steel;

  async go(id: string): Promise<void> {
    await this.client.sessions.release(id);
  }
}
