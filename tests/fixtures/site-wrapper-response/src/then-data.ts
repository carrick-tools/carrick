import { getThen } from "./then-data-lib";
export const sizes = () => getThen<{ size: number }>("/sizes");
