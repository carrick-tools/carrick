import { retried } from "./keep-retried-lib";
export const again = () => retried<{ n: number }>("/tasks");
