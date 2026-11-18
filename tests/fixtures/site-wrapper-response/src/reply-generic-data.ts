import { getJson } from "./reply-generic-data-lib";
export const prices = () => getJson<{ price: number }>("/prices");
