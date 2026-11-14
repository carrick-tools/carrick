import { route } from "fixture-router";
import { stats } from "./handlers";

route("/admin", stats);
