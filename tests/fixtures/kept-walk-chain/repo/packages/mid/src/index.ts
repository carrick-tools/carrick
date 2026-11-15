import { send } from "@fixture/base";

export type Widget = {
  id: string;
  name: string;
  activeCount: number;
};

export function getItem(id: string): Promise<Widget> {
  return send<Widget>(`/api/v1/widgets/${id}`, { method: "GET" });
}
