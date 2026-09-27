import { create } from "http-client";

const api = create();

type Scope = "all" | "specific";
interface Holiday {
  id: string;
  scope: Scope;
}
declare function setHolidays(next: Holiday[]): void;

export async function loadHolidays(): Promise<void> {
  const response = await api.get("/holidays");
  setHolidays(response.data);
}
