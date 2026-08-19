import * as telemetry from "telemetry-sink";

const sink = telemetry.createSink({ app: "api" });

export function record(name: string): void {
  telemetry.emit(name);
  sink.flush();
}
