// A class whose field is set from its constructor's options, built only in
// `open(params)` as `new Panel({ token, ...params })`. `open` is a method
// of an exported class that this service never calls (another service
// does, writing `onEvent`), so the field's value is left to callers the
// resolver does not find: it is unread, not unbound.
export type PanelProps = { token?: string; onEvent?: (name: string) => void };

export class Panel {
  private token;
  private onEvent;

  constructor({ token, onEvent }: PanelProps) {
    this.token = token;
    this.onEvent = onEvent;
  }

  emit(name: string): void {
    if (this.onEvent) {
      void this.onEvent(`${this.token}:${name}`);
    }
  }
}

export class Host {
  private token = 'host';

  open(params: PanelProps): Panel {
    const panel = new Panel({ token: this.token, ...params });
    panel.emit('opened');
    return panel;
  }
}
