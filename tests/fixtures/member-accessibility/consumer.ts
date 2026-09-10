import { Surface } from "./surface";
export function consume(): string {
  const surface = Surface.create();
  return surface.run();
}
