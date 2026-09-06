// One channel per install.
//
// The hook and the language server carry the same verdicts. Delivering both in
// one session means the model reads every finding twice and a measurement of
// either channel measures the pair, so exactly one of them speaks.
//
// The decision is config, not sniffing. The Claude Code plugin registers the
// hook and the server together, so its `.lsp.json` passes `--hooks-installed`
// to the server: the manifest that knows the hook exists is the one that says
// so. An editor or a generic LSP client starts the server without that flag and
// has no hook at all, so the server publishes. `CARRICK_CHANNEL` overrides both
// and is how a smoke run pins one arm.

export type Channel = "hook" | "lsp" | "off";

export type ChannelInput = {
  env?: NodeJS.ProcessEnv;
  /** True when a PostToolUse hook is registered alongside this process. */
  hooksInstalled: boolean;
};

export type ChannelChoice = {
  channel: Channel;
  /** How it was decided, for the log line. */
  source: "env" | "default";
  /** Set when CARRICK_CHANNEL held a value we do not know. */
  ignoredValue: string | null;
};

const KNOWN: Channel[] = ["hook", "lsp", "off"];

export function resolveChannel(input: ChannelInput): ChannelChoice {
  const raw = (input.env ?? process.env)["CARRICK_CHANNEL"];
  if (raw) {
    const value = raw.trim().toLowerCase();
    if ((KNOWN as string[]).includes(value)) {
      return { channel: value as Channel, source: "env", ignoredValue: null };
    }
    return {
      channel: input.hooksInstalled ? "hook" : "lsp",
      source: "default",
      ignoredValue: raw,
    };
  }
  return {
    channel: input.hooksInstalled ? "hook" : "lsp",
    source: "default",
    ignoredValue: null,
  };
}

/** Whether this process is the one that delivers. */
export function ownsDelivery(mine: Exclude<Channel, "off">, choice: ChannelChoice): boolean {
  return choice.channel === mine;
}
