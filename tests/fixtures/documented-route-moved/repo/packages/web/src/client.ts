import { send } from './send';

export class WidgetClient {
  constructor(private readonly baseUrl: string) {}

  listWidgets(): Promise<{ widgets: string[] }> {
    return send<{ widgets: string[] }>(`${this.baseUrl}/api/v1/widgets`, {
      method: 'GET',
    });
  }
}
