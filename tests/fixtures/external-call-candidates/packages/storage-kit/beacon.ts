import { MetricsClient } from "beacon-metrics";

export class Beacon {
  static client = new MetricsClient({ app: "worker" });
}

export const record = (name: string): void => {
  Beacon.client.capture(name);
};
