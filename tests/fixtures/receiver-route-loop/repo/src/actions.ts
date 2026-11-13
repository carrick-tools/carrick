// A list the code builds at runtime.
export function listActions(): string[] {
  return [String(Date.now())];
}

export function ok(file: string): boolean {
  return file.length > 0;
}
