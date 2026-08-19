import { PulseClient } from "pulse-analytics";

export class PulseReporter {
  private static _instance: PulseReporter;

  private client = new PulseClient({ app: "worker" });

  static start(): void {
    const started = new PulseReporter();

    PulseReporter._instance = started;
  }

  static get current(): PulseReporter {
    return PulseReporter._instance;
  }
}

export const flush = async (): Promise<void> => {
  const reporter = PulseReporter._instance;

  await reporter.client.shutdown();
};

export const capture = (name: string): void => {
  PulseReporter.current.client.capture(name);
};
