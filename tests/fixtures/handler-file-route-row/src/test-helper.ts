import { createRouter } from "http-server";
import { gql } from "./gql-handler";

export const testRouter = createRouter();
testRouter.all("/graphql", gql);
