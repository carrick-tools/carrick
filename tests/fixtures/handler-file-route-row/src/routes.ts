import { router } from "./router";
import { allV1 } from "./controllers/v1";
import { getItem } from "./controllers/items";
import { h } from "./controllers/self";

router.all("/v1/*", allV1);
router.get("/items/:id", getItem);
router.get("/b", h);
