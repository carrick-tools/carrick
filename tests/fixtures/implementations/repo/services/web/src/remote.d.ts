import type { Sink } from "./declared-only";

export declare class Remote implements Sink {
  emit(name: string): unknown;
}
