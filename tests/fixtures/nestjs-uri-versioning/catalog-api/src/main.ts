import { VersioningType, createApp } from "./framework";

export function bootstrap(): void {
  const app = createApp();
  app.enableVersioning({ type: VersioningType.URI, defaultVersion: "1" });
}
