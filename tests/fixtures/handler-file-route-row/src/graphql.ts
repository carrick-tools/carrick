import { router } from "./router";
import { gql } from "./gql-handler";

router.all("/graphql", gql);
