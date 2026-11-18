import { api } from "./seed-promise-class-lib";
export const queried = () => api.executeQuery("select 1");
