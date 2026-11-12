import { format } from "fixture-answered";

export function render(body: unknown): string {
  return format(body);
}

export function show(body: unknown): string {
  return render(body);
}
