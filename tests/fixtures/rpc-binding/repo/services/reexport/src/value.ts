import { useFn } from "@fixture/rpc";
import { saveNote } from "./notes.functions";

export function register(handler: unknown): void {
  void handler;
}

export function wire(): void {
  const f = useFn(saveNote);
  register(f);
}
