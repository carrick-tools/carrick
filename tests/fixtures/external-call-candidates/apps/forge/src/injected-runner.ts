import type Forge from "forge-sdk";

export class InjectedRunner {
  private readonly client!: Forge;

  async go(id: string): Promise<void> {
    await this.client.sessions.release(id);
  }
}
