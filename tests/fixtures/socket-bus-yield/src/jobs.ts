class Queue {
  to(_name: string): this {
    return this;
  }
  emit(_event: string, _payload: unknown): boolean {
    return true;
  }
}

export function finish(queue: Queue, name: string, result: string): void {
  queue
    .to(name)
    .emit("job.done", result);
}
