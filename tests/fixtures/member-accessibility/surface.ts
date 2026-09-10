export class Surface {
  public run(): string { return this.hidden(); }
  private hidden(): string { return "internal"; }
  protected inherited(): string { return "subclass"; }
  #secret(): string { return "private"; }
  public static create(): Surface { return new Surface(); }
  private static reset(): void {}
  protected get state(): string { return "internal"; }
  private action = (): string => this.hidden();
  #privateAction = (): string => this.#secret();
}
