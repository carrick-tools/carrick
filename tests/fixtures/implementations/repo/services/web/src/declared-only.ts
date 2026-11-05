import { Remote } from "./remote";

export interface Sink {
  emit(name: string): unknown;
}

const sink: Sink = new Remote();

export function emitOnce(): unknown {
  return sink.emit("only-declared");
}
