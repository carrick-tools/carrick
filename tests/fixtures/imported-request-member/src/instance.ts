// A module that constructs the client and exports the INSTANCE. A consumer
// holds it as an imported binding whose source module is not the member's, so
// the name join declines it by design: nothing in the name says this binding
// is the client (carrick#656).
import { ApiClient } from "./apiClient.js";

export const client = new ApiClient("https://api.example.test");
