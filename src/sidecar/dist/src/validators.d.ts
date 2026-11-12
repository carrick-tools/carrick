/**
 * Zod validators for the sidecar message protocol
 * These validators ensure incoming JSON messages are correctly typed
 */
import { z } from 'zod';
import type { SidecarRequest, InferRequestItem, LibraryCheck } from './types.js';
export declare const InferKindSchema: z.ZodEnum<["function_return", "expression", "call_result", "variable", "response_body", "request_body", "signature_return", "function_param", "receiver_type", "request_config"]>;
export declare const SymbolRequestSchema: z.ZodObject<{
    symbol_name: z.ZodString;
    source_file: z.ZodString;
    alias: z.ZodOptional<z.ZodString>;
    array_depth: z.ZodOptional<z.ZodNumber>;
    consumer_response: z.ZodOptional<z.ZodBoolean>;
}, "strip", z.ZodTypeAny, {
    symbol_name: string;
    source_file: string;
    consumer_response?: boolean | undefined;
    alias?: string | undefined;
    array_depth?: number | undefined;
}, {
    symbol_name: string;
    source_file: string;
    consumer_response?: boolean | undefined;
    alias?: string | undefined;
    array_depth?: number | undefined;
}>;
/**
 * The library store's label for which part of a call's result holds the
 * response body (carrick#2818). Exactly one key.
 */
export declare const ResultLabelSchema: z.ZodUnion<[z.ZodObject<{
    whole: z.ZodLiteral<true>;
}, "strict", z.ZodTypeAny, {
    whole: true;
}, {
    whole: true;
}>, z.ZodObject<{
    member: z.ZodTuple<[z.ZodString], null>;
}, "strict", z.ZodTypeAny, {
    member: [string];
}, {
    member: [string];
}>, z.ZodObject<{
    parse: z.ZodString;
}, "strict", z.ZodTypeAny, {
    parse: string;
}, {
    parse: string;
}>]>;
export declare const InferRequestItemSchema: z.ZodObject<{
    file_path: z.ZodString;
    line_number: z.ZodNumber;
    span_start: z.ZodOptional<z.ZodNumber>;
    span_end: z.ZodOptional<z.ZodNumber>;
    expression_text: z.ZodOptional<z.ZodString>;
    expression_line: z.ZodOptional<z.ZodNumber>;
    infer_kind: z.ZodEnum<["function_return", "expression", "call_result", "variable", "response_body", "request_body", "signature_return", "function_param", "receiver_type", "request_config"]>;
    alias: z.ZodOptional<z.ZodString>;
    param_name: z.ZodOptional<z.ZodString>;
    member: z.ZodOptional<z.ZodString>;
    result: z.ZodOptional<z.ZodUnion<[z.ZodObject<{
        whole: z.ZodLiteral<true>;
    }, "strict", z.ZodTypeAny, {
        whole: true;
    }, {
        whole: true;
    }>, z.ZodObject<{
        member: z.ZodTuple<[z.ZodString], null>;
    }, "strict", z.ZodTypeAny, {
        member: [string];
    }, {
        member: [string];
    }>, z.ZodObject<{
        parse: z.ZodString;
    }, "strict", z.ZodTypeAny, {
        parse: string;
    }, {
        parse: string;
    }>]>>;
    route_path: z.ZodOptional<z.ZodString>;
}, "strip", z.ZodTypeAny, {
    file_path: string;
    line_number: number;
    infer_kind: "function_return" | "expression" | "call_result" | "variable" | "response_body" | "request_body" | "signature_return" | "function_param" | "receiver_type" | "request_config";
    member?: string | undefined;
    result?: {
        whole: true;
    } | {
        member: [string];
    } | {
        parse: string;
    } | undefined;
    param_name?: string | undefined;
    alias?: string | undefined;
    span_start?: number | undefined;
    span_end?: number | undefined;
    expression_text?: string | undefined;
    expression_line?: number | undefined;
    route_path?: string | undefined;
}, {
    file_path: string;
    line_number: number;
    infer_kind: "function_return" | "expression" | "call_result" | "variable" | "response_body" | "request_body" | "signature_return" | "function_param" | "receiver_type" | "request_config";
    member?: string | undefined;
    result?: {
        whole: true;
    } | {
        member: [string];
    } | {
        parse: string;
    } | undefined;
    param_name?: string | undefined;
    alias?: string | undefined;
    span_start?: number | undefined;
    span_end?: number | undefined;
    expression_text?: string | undefined;
    expression_line?: number | undefined;
    route_path?: string | undefined;
}>;
/**
 * Per-item locator validation for infer requests.
 *
 * This is deliberately NOT part of the envelope schema: real runs batch
 * every alias of a repo into one infer request, and a single bad item
 * must produce a per-item error (the alias then pads to `unknown`
 * downstream), not reject the whole batch.
 *
 * Returns an error message, or null when the item is valid.
 */
export declare function validateInferRequestItem(item: InferRequestItem): string | null;
export declare const InitRequestSchema: z.ZodObject<{
    request_id: z.ZodString;
} & {
    action: z.ZodLiteral<"init">;
    repo_root: z.ZodString;
    tsconfig_path: z.ZodOptional<z.ZodString>;
    scan_root: z.ZodOptional<z.ZodString>;
    tsconfig_snapshot: z.ZodOptional<z.ZodObject<{
        compilerOptions: z.ZodObject<{
            module: z.ZodOptional<z.ZodString>;
            moduleResolution: z.ZodOptional<z.ZodString>;
            target: z.ZodOptional<z.ZodString>;
            lib: z.ZodOptional<z.ZodArray<z.ZodString, "many">>;
            types: z.ZodOptional<z.ZodArray<z.ZodString, "many">>;
            typeRoots: z.ZodOptional<z.ZodArray<z.ZodString, "many">>;
            jsx: z.ZodOptional<z.ZodString>;
            strict: z.ZodOptional<z.ZodBoolean>;
            esModuleInterop: z.ZodOptional<z.ZodBoolean>;
            skipLibCheck: z.ZodOptional<z.ZodBoolean>;
            declaration: z.ZodOptional<z.ZodBoolean>;
            declarationMap: z.ZodOptional<z.ZodBoolean>;
            paths: z.ZodOptional<z.ZodRecord<z.ZodString, z.ZodArray<z.ZodString, "many">>>;
            baseUrl: z.ZodOptional<z.ZodString>;
        }, "passthrough", z.ZodTypeAny, z.objectOutputType<{
            module: z.ZodOptional<z.ZodString>;
            moduleResolution: z.ZodOptional<z.ZodString>;
            target: z.ZodOptional<z.ZodString>;
            lib: z.ZodOptional<z.ZodArray<z.ZodString, "many">>;
            types: z.ZodOptional<z.ZodArray<z.ZodString, "many">>;
            typeRoots: z.ZodOptional<z.ZodArray<z.ZodString, "many">>;
            jsx: z.ZodOptional<z.ZodString>;
            strict: z.ZodOptional<z.ZodBoolean>;
            esModuleInterop: z.ZodOptional<z.ZodBoolean>;
            skipLibCheck: z.ZodOptional<z.ZodBoolean>;
            declaration: z.ZodOptional<z.ZodBoolean>;
            declarationMap: z.ZodOptional<z.ZodBoolean>;
            paths: z.ZodOptional<z.ZodRecord<z.ZodString, z.ZodArray<z.ZodString, "many">>>;
            baseUrl: z.ZodOptional<z.ZodString>;
        }, z.ZodTypeAny, "passthrough">, z.objectInputType<{
            module: z.ZodOptional<z.ZodString>;
            moduleResolution: z.ZodOptional<z.ZodString>;
            target: z.ZodOptional<z.ZodString>;
            lib: z.ZodOptional<z.ZodArray<z.ZodString, "many">>;
            types: z.ZodOptional<z.ZodArray<z.ZodString, "many">>;
            typeRoots: z.ZodOptional<z.ZodArray<z.ZodString, "many">>;
            jsx: z.ZodOptional<z.ZodString>;
            strict: z.ZodOptional<z.ZodBoolean>;
            esModuleInterop: z.ZodOptional<z.ZodBoolean>;
            skipLibCheck: z.ZodOptional<z.ZodBoolean>;
            declaration: z.ZodOptional<z.ZodBoolean>;
            declarationMap: z.ZodOptional<z.ZodBoolean>;
            paths: z.ZodOptional<z.ZodRecord<z.ZodString, z.ZodArray<z.ZodString, "many">>>;
            baseUrl: z.ZodOptional<z.ZodString>;
        }, z.ZodTypeAny, "passthrough">>;
    }, "strip", z.ZodTypeAny, {
        compilerOptions: {
            module?: string | undefined;
            moduleResolution?: string | undefined;
            target?: string | undefined;
            lib?: string[] | undefined;
            types?: string[] | undefined;
            typeRoots?: string[] | undefined;
            jsx?: string | undefined;
            strict?: boolean | undefined;
            esModuleInterop?: boolean | undefined;
            skipLibCheck?: boolean | undefined;
            declaration?: boolean | undefined;
            declarationMap?: boolean | undefined;
            paths?: Record<string, string[]> | undefined;
            baseUrl?: string | undefined;
        } & {
            [k: string]: unknown;
        };
    }, {
        compilerOptions: {
            module?: string | undefined;
            moduleResolution?: string | undefined;
            target?: string | undefined;
            lib?: string[] | undefined;
            types?: string[] | undefined;
            typeRoots?: string[] | undefined;
            jsx?: string | undefined;
            strict?: boolean | undefined;
            esModuleInterop?: boolean | undefined;
            skipLibCheck?: boolean | undefined;
            declaration?: boolean | undefined;
            declarationMap?: boolean | undefined;
            paths?: Record<string, string[]> | undefined;
            baseUrl?: string | undefined;
        } & {
            [k: string]: unknown;
        };
    }>>;
    pinned_dependencies: z.ZodOptional<z.ZodRecord<z.ZodString, z.ZodString>>;
    workspace_source: z.ZodOptional<z.ZodBoolean>;
}, "strip", z.ZodTypeAny, {
    request_id: string;
    action: "init";
    repo_root: string;
    tsconfig_path?: string | undefined;
    scan_root?: string | undefined;
    tsconfig_snapshot?: {
        compilerOptions: {
            module?: string | undefined;
            moduleResolution?: string | undefined;
            target?: string | undefined;
            lib?: string[] | undefined;
            types?: string[] | undefined;
            typeRoots?: string[] | undefined;
            jsx?: string | undefined;
            strict?: boolean | undefined;
            esModuleInterop?: boolean | undefined;
            skipLibCheck?: boolean | undefined;
            declaration?: boolean | undefined;
            declarationMap?: boolean | undefined;
            paths?: Record<string, string[]> | undefined;
            baseUrl?: string | undefined;
        } & {
            [k: string]: unknown;
        };
    } | undefined;
    pinned_dependencies?: Record<string, string> | undefined;
    workspace_source?: boolean | undefined;
}, {
    request_id: string;
    action: "init";
    repo_root: string;
    tsconfig_path?: string | undefined;
    scan_root?: string | undefined;
    tsconfig_snapshot?: {
        compilerOptions: {
            module?: string | undefined;
            moduleResolution?: string | undefined;
            target?: string | undefined;
            lib?: string[] | undefined;
            types?: string[] | undefined;
            typeRoots?: string[] | undefined;
            jsx?: string | undefined;
            strict?: boolean | undefined;
            esModuleInterop?: boolean | undefined;
            skipLibCheck?: boolean | undefined;
            declaration?: boolean | undefined;
            declarationMap?: boolean | undefined;
            paths?: Record<string, string[]> | undefined;
            baseUrl?: string | undefined;
        } & {
            [k: string]: unknown;
        };
    } | undefined;
    pinned_dependencies?: Record<string, string> | undefined;
    workspace_source?: boolean | undefined;
}>;
export declare const BundleRequestSchema: z.ZodObject<{
    request_id: z.ZodString;
} & {
    action: z.ZodLiteral<"bundle">;
    symbols: z.ZodArray<z.ZodObject<{
        symbol_name: z.ZodString;
        source_file: z.ZodString;
        alias: z.ZodOptional<z.ZodString>;
        array_depth: z.ZodOptional<z.ZodNumber>;
        consumer_response: z.ZodOptional<z.ZodBoolean>;
    }, "strip", z.ZodTypeAny, {
        symbol_name: string;
        source_file: string;
        consumer_response?: boolean | undefined;
        alias?: string | undefined;
        array_depth?: number | undefined;
    }, {
        symbol_name: string;
        source_file: string;
        consumer_response?: boolean | undefined;
        alias?: string | undefined;
        array_depth?: number | undefined;
    }>, "many">;
}, "strip", z.ZodTypeAny, {
    request_id: string;
    action: "bundle";
    symbols: {
        symbol_name: string;
        source_file: string;
        consumer_response?: boolean | undefined;
        alias?: string | undefined;
        array_depth?: number | undefined;
    }[];
}, {
    request_id: string;
    action: "bundle";
    symbols: {
        symbol_name: string;
        source_file: string;
        consumer_response?: boolean | undefined;
        alias?: string | undefined;
        array_depth?: number | undefined;
    }[];
}>;
export declare const CaptureV2RequestSchema: z.ZodObject<{
    request_id: z.ZodString;
} & {
    action: z.ZodLiteral<"capture_v2">;
    repo_root: z.ZodString;
    service_name: z.ZodString;
    anchors: z.ZodArray<z.ZodDiscriminatedUnion<"kind", [z.ZodObject<{
        kind: z.ZodLiteral<"symbol">;
        alias: z.ZodString;
        symbol_name: z.ZodString;
        source_file: z.ZodString;
        anchor_origin: z.ZodEnum<["llm-symbol", "deterministic-infer", "anchor-backfill", "manifest-placeholder"]>;
        array_depth: z.ZodOptional<z.ZodNumber>;
        consumer_response: z.ZodOptional<z.ZodBoolean>;
        absent_body_tolerated: z.ZodOptional<z.ZodLiteral<true>>;
    }, "strip", z.ZodTypeAny, {
        kind: "symbol";
        alias: string;
        symbol_name: string;
        source_file: string;
        anchor_origin: "llm-symbol" | "deterministic-infer" | "anchor-backfill" | "manifest-placeholder";
        consumer_response?: boolean | undefined;
        absent_body_tolerated?: true | undefined;
        array_depth?: number | undefined;
    }, {
        kind: "symbol";
        alias: string;
        symbol_name: string;
        source_file: string;
        anchor_origin: "llm-symbol" | "deterministic-infer" | "anchor-backfill" | "manifest-placeholder";
        consumer_response?: boolean | undefined;
        absent_body_tolerated?: true | undefined;
        array_depth?: number | undefined;
    }>, z.ZodObject<{
        kind: z.ZodLiteral<"handler_return">;
        alias: z.ZodString;
        symbol_name: z.ZodString;
        source_file: z.ZodString;
        anchor_origin: z.ZodEnum<["llm-symbol", "deterministic-infer", "anchor-backfill", "manifest-placeholder"]>;
    }, "strip", z.ZodTypeAny, {
        kind: "handler_return";
        alias: string;
        symbol_name: string;
        source_file: string;
        anchor_origin: "llm-symbol" | "deterministic-infer" | "anchor-backfill" | "manifest-placeholder";
    }, {
        kind: "handler_return";
        alias: string;
        symbol_name: string;
        source_file: string;
        anchor_origin: "llm-symbol" | "deterministic-infer" | "anchor-backfill" | "manifest-placeholder";
    }>, z.ZodObject<{
        kind: z.ZodLiteral<"infer">;
        alias: z.ZodString;
        source_file: z.ZodString;
        anchor_origin: z.ZodEnum<["llm-symbol", "deterministic-infer", "anchor-backfill", "manifest-placeholder"]>;
        span_start: z.ZodOptional<z.ZodNumber>;
        span_end: z.ZodOptional<z.ZodNumber>;
        line_number: z.ZodOptional<z.ZodNumber>;
        expression_text: z.ZodOptional<z.ZodString>;
        param_name: z.ZodOptional<z.ZodString>;
        unwrap: z.ZodOptional<z.ZodEnum<["awaited", "none"]>>;
        result: z.ZodOptional<z.ZodUnion<[z.ZodObject<{
            whole: z.ZodLiteral<true>;
        }, "strict", z.ZodTypeAny, {
            whole: true;
        }, {
            whole: true;
        }>, z.ZodObject<{
            member: z.ZodTuple<[z.ZodString], null>;
        }, "strict", z.ZodTypeAny, {
            member: [string];
        }, {
            member: [string];
        }>, z.ZodObject<{
            parse: z.ZodString;
        }, "strict", z.ZodTypeAny, {
            parse: string;
        }, {
            parse: string;
        }>]>>;
    }, "strip", z.ZodTypeAny, {
        kind: "infer";
        alias: string;
        source_file: string;
        anchor_origin: "llm-symbol" | "deterministic-infer" | "anchor-backfill" | "manifest-placeholder";
        result?: {
            whole: true;
        } | {
            member: [string];
        } | {
            parse: string;
        } | undefined;
        param_name?: string | undefined;
        line_number?: number | undefined;
        span_start?: number | undefined;
        span_end?: number | undefined;
        expression_text?: string | undefined;
        unwrap?: "awaited" | "none" | undefined;
    }, {
        kind: "infer";
        alias: string;
        source_file: string;
        anchor_origin: "llm-symbol" | "deterministic-infer" | "anchor-backfill" | "manifest-placeholder";
        result?: {
            whole: true;
        } | {
            member: [string];
        } | {
            parse: string;
        } | undefined;
        param_name?: string | undefined;
        line_number?: number | undefined;
        span_start?: number | undefined;
        span_end?: number | undefined;
        expression_text?: string | undefined;
        unwrap?: "awaited" | "none" | undefined;
    }>, z.ZodObject<{
        kind: z.ZodLiteral<"literal">;
        alias: z.ZodString;
        type_text: z.ZodString;
        anchor_origin: z.ZodEnum<["llm-symbol", "deterministic-infer", "anchor-backfill", "manifest-placeholder"]>;
        source_file: z.ZodOptional<z.ZodString>;
        printed_names: z.ZodOptional<z.ZodArray<z.ZodObject<{
            name: z.ZodString;
            file: z.ZodString;
            export_path: z.ZodArray<z.ZodString, "many">;
        }, "strip", z.ZodTypeAny, {
            name: string;
            file: string;
            export_path: string[];
        }, {
            name: string;
            file: string;
            export_path: string[];
        }>, "many">>;
        raw_text_read: z.ZodOptional<z.ZodLiteral<true>>;
        absent_body_tolerated: z.ZodOptional<z.ZodLiteral<true>>;
    }, "strip", z.ZodTypeAny, {
        kind: "literal";
        alias: string;
        anchor_origin: "llm-symbol" | "deterministic-infer" | "anchor-backfill" | "manifest-placeholder";
        type_text: string;
        raw_text_read?: true | undefined;
        absent_body_tolerated?: true | undefined;
        source_file?: string | undefined;
        printed_names?: {
            name: string;
            file: string;
            export_path: string[];
        }[] | undefined;
    }, {
        kind: "literal";
        alias: string;
        anchor_origin: "llm-symbol" | "deterministic-infer" | "anchor-backfill" | "manifest-placeholder";
        type_text: string;
        raw_text_read?: true | undefined;
        absent_body_tolerated?: true | undefined;
        source_file?: string | undefined;
        printed_names?: {
            name: string;
            file: string;
            export_path: string[];
        }[] | undefined;
    }>]>, "many">;
    out_dir: z.ZodString;
    tsconfig_path: z.ZodOptional<z.ZodString>;
    scan_root: z.ZodOptional<z.ZodString>;
    backfill_texts: z.ZodOptional<z.ZodRecord<z.ZodString, z.ZodString>>;
    backfill_out_dir: z.ZodOptional<z.ZodString>;
}, "strip", z.ZodTypeAny, {
    anchors: ({
        kind: "symbol";
        alias: string;
        symbol_name: string;
        source_file: string;
        anchor_origin: "llm-symbol" | "deterministic-infer" | "anchor-backfill" | "manifest-placeholder";
        consumer_response?: boolean | undefined;
        absent_body_tolerated?: true | undefined;
        array_depth?: number | undefined;
    } | {
        kind: "handler_return";
        alias: string;
        symbol_name: string;
        source_file: string;
        anchor_origin: "llm-symbol" | "deterministic-infer" | "anchor-backfill" | "manifest-placeholder";
    } | {
        kind: "infer";
        alias: string;
        source_file: string;
        anchor_origin: "llm-symbol" | "deterministic-infer" | "anchor-backfill" | "manifest-placeholder";
        result?: {
            whole: true;
        } | {
            member: [string];
        } | {
            parse: string;
        } | undefined;
        param_name?: string | undefined;
        line_number?: number | undefined;
        span_start?: number | undefined;
        span_end?: number | undefined;
        expression_text?: string | undefined;
        unwrap?: "awaited" | "none" | undefined;
    } | {
        kind: "literal";
        alias: string;
        anchor_origin: "llm-symbol" | "deterministic-infer" | "anchor-backfill" | "manifest-placeholder";
        type_text: string;
        raw_text_read?: true | undefined;
        absent_body_tolerated?: true | undefined;
        source_file?: string | undefined;
        printed_names?: {
            name: string;
            file: string;
            export_path: string[];
        }[] | undefined;
    })[];
    request_id: string;
    action: "capture_v2";
    repo_root: string;
    service_name: string;
    out_dir: string;
    tsconfig_path?: string | undefined;
    scan_root?: string | undefined;
    backfill_texts?: Record<string, string> | undefined;
    backfill_out_dir?: string | undefined;
}, {
    anchors: ({
        kind: "symbol";
        alias: string;
        symbol_name: string;
        source_file: string;
        anchor_origin: "llm-symbol" | "deterministic-infer" | "anchor-backfill" | "manifest-placeholder";
        consumer_response?: boolean | undefined;
        absent_body_tolerated?: true | undefined;
        array_depth?: number | undefined;
    } | {
        kind: "handler_return";
        alias: string;
        symbol_name: string;
        source_file: string;
        anchor_origin: "llm-symbol" | "deterministic-infer" | "anchor-backfill" | "manifest-placeholder";
    } | {
        kind: "infer";
        alias: string;
        source_file: string;
        anchor_origin: "llm-symbol" | "deterministic-infer" | "anchor-backfill" | "manifest-placeholder";
        result?: {
            whole: true;
        } | {
            member: [string];
        } | {
            parse: string;
        } | undefined;
        param_name?: string | undefined;
        line_number?: number | undefined;
        span_start?: number | undefined;
        span_end?: number | undefined;
        expression_text?: string | undefined;
        unwrap?: "awaited" | "none" | undefined;
    } | {
        kind: "literal";
        alias: string;
        anchor_origin: "llm-symbol" | "deterministic-infer" | "anchor-backfill" | "manifest-placeholder";
        type_text: string;
        raw_text_read?: true | undefined;
        absent_body_tolerated?: true | undefined;
        source_file?: string | undefined;
        printed_names?: {
            name: string;
            file: string;
            export_path: string[];
        }[] | undefined;
    })[];
    request_id: string;
    action: "capture_v2";
    repo_root: string;
    service_name: string;
    out_dir: string;
    tsconfig_path?: string | undefined;
    scan_root?: string | undefined;
    backfill_texts?: Record<string, string> | undefined;
    backfill_out_dir?: string | undefined;
}>;
export declare const CheckV2RequestSchema: z.ZodObject<{
    request_id: z.ZodString;
} & {
    action: z.ZodLiteral<"check_v2">;
    stubs: z.ZodArray<z.ZodObject<{
        service_name: z.ZodString;
        stub_dir: z.ZodString;
    }, "strip", z.ZodTypeAny, {
        service_name: string;
        stub_dir: string;
    }, {
        service_name: string;
        stub_dir: string;
    }>, "many">;
    pairs: z.ZodArray<z.ZodEffects<z.ZodObject<{
        pair_key: z.ZodString;
        protocol: z.ZodEnum<["http", "graphql", "socket", "pubsub"]>;
        type_kind: z.ZodEnum<["request", "response", "both"]>;
        producer: z.ZodObject<{
            service_name: z.ZodString;
            alias: z.ZodString;
        }, "strip", z.ZodTypeAny, {
            alias: string;
            service_name: string;
        }, {
            alias: string;
            service_name: string;
        }>;
        consumer: z.ZodObject<{
            service_name: z.ZodString;
            alias: z.ZodString;
        }, "strip", z.ZodTypeAny, {
            alias: string;
            service_name: string;
        }, {
            alias: string;
            service_name: string;
        }>;
        topic: z.ZodOptional<z.ZodString>;
    }, "strip", z.ZodTypeAny, {
        producer: {
            alias: string;
            service_name: string;
        };
        consumer: {
            alias: string;
            service_name: string;
        };
        protocol: "http" | "graphql" | "socket" | "pubsub";
        pair_key: string;
        type_kind: "request" | "response" | "both";
        topic?: string | undefined;
    }, {
        producer: {
            alias: string;
            service_name: string;
        };
        consumer: {
            alias: string;
            service_name: string;
        };
        protocol: "http" | "graphql" | "socket" | "pubsub";
        pair_key: string;
        type_kind: "request" | "response" | "both";
        topic?: string | undefined;
    }>, {
        producer: {
            alias: string;
            service_name: string;
        };
        consumer: {
            alias: string;
            service_name: string;
        };
        protocol: "http" | "graphql" | "socket" | "pubsub";
        pair_key: string;
        type_kind: "request" | "response" | "both";
        topic?: string | undefined;
    }, {
        producer: {
            alias: string;
            service_name: string;
        };
        consumer: {
            alias: string;
            service_name: string;
        };
        protocol: "http" | "graphql" | "socket" | "pubsub";
        pair_key: string;
        type_kind: "request" | "response" | "both";
        topic?: string | undefined;
    }>, "many">;
    workspace_root: z.ZodOptional<z.ZodString>;
    keep_workspace: z.ZodOptional<z.ZodBoolean>;
}, "strip", z.ZodTypeAny, {
    pairs: {
        producer: {
            alias: string;
            service_name: string;
        };
        consumer: {
            alias: string;
            service_name: string;
        };
        protocol: "http" | "graphql" | "socket" | "pubsub";
        pair_key: string;
        type_kind: "request" | "response" | "both";
        topic?: string | undefined;
    }[];
    request_id: string;
    action: "check_v2";
    stubs: {
        service_name: string;
        stub_dir: string;
    }[];
    workspace_root?: string | undefined;
    keep_workspace?: boolean | undefined;
}, {
    pairs: {
        producer: {
            alias: string;
            service_name: string;
        };
        consumer: {
            alias: string;
            service_name: string;
        };
        protocol: "http" | "graphql" | "socket" | "pubsub";
        pair_key: string;
        type_kind: "request" | "response" | "both";
        topic?: string | undefined;
    }[];
    request_id: string;
    action: "check_v2";
    stubs: {
        service_name: string;
        stub_dir: string;
    }[];
    workspace_root?: string | undefined;
    keep_workspace?: boolean | undefined;
}>;
export declare const InferRequestSchema: z.ZodObject<{
    request_id: z.ZodString;
} & {
    action: z.ZodLiteral<"infer">;
    requests: z.ZodArray<z.ZodObject<{
        file_path: z.ZodString;
        line_number: z.ZodNumber;
        span_start: z.ZodOptional<z.ZodNumber>;
        span_end: z.ZodOptional<z.ZodNumber>;
        expression_text: z.ZodOptional<z.ZodString>;
        expression_line: z.ZodOptional<z.ZodNumber>;
        infer_kind: z.ZodEnum<["function_return", "expression", "call_result", "variable", "response_body", "request_body", "signature_return", "function_param", "receiver_type", "request_config"]>;
        alias: z.ZodOptional<z.ZodString>;
        param_name: z.ZodOptional<z.ZodString>;
        member: z.ZodOptional<z.ZodString>;
        result: z.ZodOptional<z.ZodUnion<[z.ZodObject<{
            whole: z.ZodLiteral<true>;
        }, "strict", z.ZodTypeAny, {
            whole: true;
        }, {
            whole: true;
        }>, z.ZodObject<{
            member: z.ZodTuple<[z.ZodString], null>;
        }, "strict", z.ZodTypeAny, {
            member: [string];
        }, {
            member: [string];
        }>, z.ZodObject<{
            parse: z.ZodString;
        }, "strict", z.ZodTypeAny, {
            parse: string;
        }, {
            parse: string;
        }>]>>;
        route_path: z.ZodOptional<z.ZodString>;
    }, "strip", z.ZodTypeAny, {
        file_path: string;
        line_number: number;
        infer_kind: "function_return" | "expression" | "call_result" | "variable" | "response_body" | "request_body" | "signature_return" | "function_param" | "receiver_type" | "request_config";
        member?: string | undefined;
        result?: {
            whole: true;
        } | {
            member: [string];
        } | {
            parse: string;
        } | undefined;
        param_name?: string | undefined;
        alias?: string | undefined;
        span_start?: number | undefined;
        span_end?: number | undefined;
        expression_text?: string | undefined;
        expression_line?: number | undefined;
        route_path?: string | undefined;
    }, {
        file_path: string;
        line_number: number;
        infer_kind: "function_return" | "expression" | "call_result" | "variable" | "response_body" | "request_body" | "signature_return" | "function_param" | "receiver_type" | "request_config";
        member?: string | undefined;
        result?: {
            whole: true;
        } | {
            member: [string];
        } | {
            parse: string;
        } | undefined;
        param_name?: string | undefined;
        alias?: string | undefined;
        span_start?: number | undefined;
        span_end?: number | undefined;
        expression_text?: string | undefined;
        expression_line?: number | undefined;
        route_path?: string | undefined;
    }>, "many">;
    extraction_config: z.ZodOptional<z.ZodObject<{
        rules: z.ZodArray<z.ZodObject<{
            wrapperSymbols: z.ZodOptional<z.ZodArray<z.ZodString, "many">>;
            machineryIndicators: z.ZodOptional<z.ZodArray<z.ZodString, "many">>;
            originModuleGlobs: z.ZodOptional<z.ZodArray<z.ZodString, "many">>;
            payloadGenericIndex: z.ZodOptional<z.ZodNumber>;
            payloadPropertyPath: z.ZodOptional<z.ZodArray<z.ZodString, "many">>;
            unwrapRecursively: z.ZodOptional<z.ZodBoolean>;
            maxDepth: z.ZodOptional<z.ZodNumber>;
        }, "strip", z.ZodTypeAny, {
            wrapperSymbols?: string[] | undefined;
            machineryIndicators?: string[] | undefined;
            originModuleGlobs?: string[] | undefined;
            payloadGenericIndex?: number | undefined;
            payloadPropertyPath?: string[] | undefined;
            unwrapRecursively?: boolean | undefined;
            maxDepth?: number | undefined;
        }, {
            wrapperSymbols?: string[] | undefined;
            machineryIndicators?: string[] | undefined;
            originModuleGlobs?: string[] | undefined;
            payloadGenericIndex?: number | undefined;
            payloadPropertyPath?: string[] | undefined;
            unwrapRecursively?: boolean | undefined;
            maxDepth?: number | undefined;
        }>, "many">;
    }, "strip", z.ZodTypeAny, {
        rules: {
            wrapperSymbols?: string[] | undefined;
            machineryIndicators?: string[] | undefined;
            originModuleGlobs?: string[] | undefined;
            payloadGenericIndex?: number | undefined;
            payloadPropertyPath?: string[] | undefined;
            unwrapRecursively?: boolean | undefined;
            maxDepth?: number | undefined;
        }[];
    }, {
        rules: {
            wrapperSymbols?: string[] | undefined;
            machineryIndicators?: string[] | undefined;
            originModuleGlobs?: string[] | undefined;
            payloadGenericIndex?: number | undefined;
            payloadPropertyPath?: string[] | undefined;
            unwrapRecursively?: boolean | undefined;
            maxDepth?: number | undefined;
        }[];
    }>>;
    model_symbols: z.ZodOptional<z.ZodArray<z.ZodObject<{
        alias: z.ZodString;
        symbol_name: z.ZodString;
        source_file: z.ZodString;
    }, "strip", z.ZodTypeAny, {
        alias: string;
        symbol_name: string;
        source_file: string;
    }, {
        alias: string;
        symbol_name: string;
        source_file: string;
    }>, "many">>;
    server_claims: z.ZodOptional<z.ZodLazy<z.ZodObject<{
        from_dir: z.ZodString;
        scan_root: z.ZodOptional<z.ZodString>;
        checks: z.ZodArray<z.ZodUnknown, "many">;
        type_packages: z.ZodOptional<z.ZodRecord<z.ZodString, z.ZodArray<z.ZodString, "many">>>;
    }, "strict", z.ZodTypeAny, {
        from_dir: string;
        checks: unknown[];
        scan_root?: string | undefined;
        type_packages?: Record<string, string[]> | undefined;
    }, {
        from_dir: string;
        checks: unknown[];
        scan_root?: string | undefined;
        type_packages?: Record<string, string[]> | undefined;
    }>>>;
    framework_packages: z.ZodOptional<z.ZodArray<z.ZodString, "many">>;
}, "strip", z.ZodTypeAny, {
    request_id: string;
    action: "infer";
    requests: {
        file_path: string;
        line_number: number;
        infer_kind: "function_return" | "expression" | "call_result" | "variable" | "response_body" | "request_body" | "signature_return" | "function_param" | "receiver_type" | "request_config";
        member?: string | undefined;
        result?: {
            whole: true;
        } | {
            member: [string];
        } | {
            parse: string;
        } | undefined;
        param_name?: string | undefined;
        alias?: string | undefined;
        span_start?: number | undefined;
        span_end?: number | undefined;
        expression_text?: string | undefined;
        expression_line?: number | undefined;
        route_path?: string | undefined;
    }[];
    extraction_config?: {
        rules: {
            wrapperSymbols?: string[] | undefined;
            machineryIndicators?: string[] | undefined;
            originModuleGlobs?: string[] | undefined;
            payloadGenericIndex?: number | undefined;
            payloadPropertyPath?: string[] | undefined;
            unwrapRecursively?: boolean | undefined;
            maxDepth?: number | undefined;
        }[];
    } | undefined;
    model_symbols?: {
        alias: string;
        symbol_name: string;
        source_file: string;
    }[] | undefined;
    server_claims?: {
        from_dir: string;
        checks: unknown[];
        scan_root?: string | undefined;
        type_packages?: Record<string, string[]> | undefined;
    } | undefined;
    framework_packages?: string[] | undefined;
}, {
    request_id: string;
    action: "infer";
    requests: {
        file_path: string;
        line_number: number;
        infer_kind: "function_return" | "expression" | "call_result" | "variable" | "response_body" | "request_body" | "signature_return" | "function_param" | "receiver_type" | "request_config";
        member?: string | undefined;
        result?: {
            whole: true;
        } | {
            member: [string];
        } | {
            parse: string;
        } | undefined;
        param_name?: string | undefined;
        alias?: string | undefined;
        span_start?: number | undefined;
        span_end?: number | undefined;
        expression_text?: string | undefined;
        expression_line?: number | undefined;
        route_path?: string | undefined;
    }[];
    extraction_config?: {
        rules: {
            wrapperSymbols?: string[] | undefined;
            machineryIndicators?: string[] | undefined;
            originModuleGlobs?: string[] | undefined;
            payloadGenericIndex?: number | undefined;
            payloadPropertyPath?: string[] | undefined;
            unwrapRecursively?: boolean | undefined;
            maxDepth?: number | undefined;
        }[];
    } | undefined;
    model_symbols?: {
        alias: string;
        symbol_name: string;
        source_file: string;
    }[] | undefined;
    server_claims?: {
        from_dir: string;
        checks: unknown[];
        scan_root?: string | undefined;
        type_packages?: Record<string, string[]> | undefined;
    } | undefined;
    framework_packages?: string[] | undefined;
}>;
export declare const HealthRequestSchema: z.ZodObject<{
    request_id: z.ZodString;
} & {
    action: z.ZodLiteral<"health">;
}, "strip", z.ZodTypeAny, {
    request_id: string;
    action: "health";
}, {
    request_id: string;
    action: "health";
}>;
export declare const ShutdownRequestSchema: z.ZodObject<{
    request_id: z.ZodString;
} & {
    action: z.ZodLiteral<"shutdown">;
}, "strip", z.ZodTypeAny, {
    request_id: string;
    action: "shutdown";
}, {
    request_id: string;
    action: "shutdown";
}>;
export declare const ListProgramFilesRequestSchema: z.ZodObject<{
    request_id: z.ZodString;
} & {
    action: z.ZodLiteral<"list_program_files">;
}, "strip", z.ZodTypeAny, {
    request_id: string;
    action: "list_program_files";
}, {
    request_id: string;
    action: "list_program_files";
}>;
export declare const ReachGraphRequestSchema: z.ZodObject<{
    request_id: z.ZodString;
} & {
    action: z.ZodLiteral<"reach_graph">;
}, "strip", z.ZodTypeAny, {
    request_id: string;
    action: "reach_graph";
}, {
    request_id: string;
    action: "reach_graph";
}>;
export declare const ReachRequestSchema: z.ZodObject<{
    request_id: z.ZodString;
} & {
    action: z.ZodLiteral<"reach">;
    graph: z.ZodObject<{
        version: z.ZodLiteral<1>;
        strings: z.ZodArray<z.ZodString, "many">;
        files: z.ZodArray<z.ZodObject<{
            p: z.ZodNumber;
            g: z.ZodUnion<[z.ZodLiteral<0>, z.ZodLiteral<1>]>;
            d: z.ZodArray<z.ZodNumber, "many">;
            r: z.ZodArray<z.ZodArray<z.ZodNumber, "many">, "many">;
            l: z.ZodArray<z.ZodArray<z.ZodNumber, "many">, "many">;
            e: z.ZodArray<z.ZodNumber, "many">;
            t: z.ZodArray<z.ZodArray<z.ZodNumber, "many">, "many">;
            b: z.ZodArray<z.ZodTuple<[z.ZodNumber, z.ZodNumber, z.ZodArray<z.ZodNumber, "many">], null>, "many">;
            w: z.ZodArray<z.ZodNumber, "many">;
            s: z.ZodArray<z.ZodTuple<[z.ZodNumber, z.ZodArray<z.ZodNumber, "many">, z.ZodArray<z.ZodNumber, "many">], null>, "many">;
        }, "strip", z.ZodTypeAny, {
            b: [number, number, number[]][];
            s: [number, number[], number[]][];
            p: number;
            g: 0 | 1;
            d: number[];
            r: number[][];
            l: number[][];
            e: number[];
            t: number[][];
            w: number[];
        }, {
            b: [number, number, number[]][];
            s: [number, number[], number[]][];
            p: number;
            g: 0 | 1;
            d: number[];
            r: number[][];
            l: number[][];
            e: number[];
            t: number[][];
            w: number[];
        }>, "many">;
    }, "strip", z.ZodTypeAny, {
        version: 1;
        files: {
            b: [number, number, number[]][];
            s: [number, number[], number[]][];
            p: number;
            g: 0 | 1;
            d: number[];
            r: number[][];
            l: number[][];
            e: number[];
            t: number[][];
            w: number[];
        }[];
        strings: string[];
    }, {
        version: 1;
        files: {
            b: [number, number, number[]][];
            s: [number, number[], number[]][];
            p: number;
            g: 0 | 1;
            d: number[];
            r: number[][];
            l: number[][];
            e: number[];
            t: number[][];
            w: number[];
        }[];
        strings: string[];
    }>;
    changes: z.ZodArray<z.ZodObject<{
        file: z.ZodString;
        before: z.ZodString;
        after: z.ZodString;
    }, "strip", z.ZodTypeAny, {
        after: string;
        before: string;
        file: string;
    }, {
        after: string;
        before: string;
        file: string;
    }>, "many">;
}, "strip", z.ZodTypeAny, {
    request_id: string;
    action: "reach";
    graph: {
        version: 1;
        files: {
            b: [number, number, number[]][];
            s: [number, number[], number[]][];
            p: number;
            g: 0 | 1;
            d: number[];
            r: number[][];
            l: number[][];
            e: number[];
            t: number[][];
            w: number[];
        }[];
        strings: string[];
    };
    changes: {
        after: string;
        before: string;
        file: string;
    }[];
}, {
    request_id: string;
    action: "reach";
    graph: {
        version: 1;
        files: {
            b: [number, number, number[]][];
            s: [number, number[], number[]][];
            p: number;
            g: 0 | 1;
            d: number[];
            r: number[][];
            l: number[][];
            e: number[];
            t: number[][];
            w: number[];
        }[];
        strings: string[];
    };
    changes: {
        after: string;
        before: string;
        file: string;
    }[];
}>;
export declare const AddProgramFilesRequestSchema: z.ZodObject<{
    request_id: z.ZodString;
} & {
    action: z.ZodLiteral<"add_program_files">;
    files: z.ZodArray<z.ZodString, "many">;
}, "strip", z.ZodTypeAny, {
    files: string[];
    request_id: string;
    action: "add_program_files";
}, {
    files: string[];
    request_id: string;
    action: "add_program_files";
}>;
export declare const PositionRowsRequestSchema: z.ZodObject<{
    request_id: z.ZodString;
} & {
    action: z.ZodLiteral<"position_rows">;
    files: z.ZodArray<z.ZodString, "many">;
    scan_root: z.ZodString;
    out_path: z.ZodString;
    service: z.ZodString;
}, "strip", z.ZodTypeAny, {
    service: string;
    files: string[];
    request_id: string;
    action: "position_rows";
    scan_root: string;
    out_path: string;
}, {
    service: string;
    files: string[];
    request_id: string;
    action: "position_rows";
    scan_root: string;
    out_path: string;
}>;
export declare const ImplementationsRequestSchema: z.ZodObject<{
    request_id: z.ZodString;
} & {
    action: z.ZodLiteral<"implementations">;
    queries: z.ZodArray<z.ZodObject<{
        file: z.ZodString;
        start: z.ZodNumber;
        end: z.ZodNumber;
    }, "strip", z.ZodTypeAny, {
        file: string;
        end: number;
        start: number;
    }, {
        file: string;
        end: number;
        start: number;
    }>, "many">;
    scan_root: z.ZodString;
}, "strip", z.ZodTypeAny, {
    queries: {
        file: string;
        end: number;
        start: number;
    }[];
    request_id: string;
    action: "implementations";
    scan_root: string;
}, {
    queries: {
        file: string;
        end: number;
        start: number;
    }[];
    request_id: string;
    action: "implementations";
    scan_root: string;
}>;
export declare const ResolveDefinitionsRequestSchema: z.ZodObject<{
    request_id: z.ZodString;
} & {
    action: z.ZodLiteral<"resolve_definitions">;
    stub_dir: z.ZodString;
    aliases: z.ZodArray<z.ZodString, "many">;
}, "strip", z.ZodTypeAny, {
    request_id: string;
    action: "resolve_definitions";
    stub_dir: string;
    aliases: string[];
}, {
    request_id: string;
    action: "resolve_definitions";
    stub_dir: string;
    aliases: string[];
}>;
export declare const RetypePrescreenRequestSchema: z.ZodObject<{
    request_id: z.ZodString;
} & {
    action: z.ZodLiteral<"retype_prescreen">;
    items: z.ZodArray<z.ZodObject<{
        item_id: z.ZodString;
        file_path: z.ZodString;
        line_number: z.ZodNumber;
        span_start: z.ZodOptional<z.ZodNumber>;
        span_end: z.ZodOptional<z.ZodNumber>;
        expression_text: z.ZodOptional<z.ZodString>;
        expression_line: z.ZodOptional<z.ZodNumber>;
    }, "strip", z.ZodTypeAny, {
        file_path: string;
        line_number: number;
        item_id: string;
        span_start?: number | undefined;
        span_end?: number | undefined;
        expression_text?: string | undefined;
        expression_line?: number | undefined;
    }, {
        file_path: string;
        line_number: number;
        item_id: string;
        span_start?: number | undefined;
        span_end?: number | undefined;
        expression_text?: string | undefined;
        expression_line?: number | undefined;
    }>, "many">;
}, "strip", z.ZodTypeAny, {
    request_id: string;
    action: "retype_prescreen";
    items: {
        file_path: string;
        line_number: number;
        item_id: string;
        span_start?: number | undefined;
        span_end?: number | undefined;
        expression_text?: string | undefined;
        expression_line?: number | undefined;
    }[];
}, {
    request_id: string;
    action: "retype_prescreen";
    items: {
        file_path: string;
        line_number: number;
        item_id: string;
        span_start?: number | undefined;
        span_end?: number | undefined;
        expression_text?: string | undefined;
        expression_line?: number | undefined;
    }[];
}>;
export declare const RetypeCheckRequestSchema: z.ZodObject<{
    request_id: z.ZodString;
} & {
    action: z.ZodLiteral<"retype_check">;
    items: z.ZodArray<z.ZodObject<{
        item_id: z.ZodString;
        file_path: z.ZodString;
        line_number: z.ZodNumber;
        span_start: z.ZodOptional<z.ZodNumber>;
        span_end: z.ZodOptional<z.ZodNumber>;
        expression_text: z.ZodOptional<z.ZodString>;
        expression_line: z.ZodOptional<z.ZodNumber>;
    } & {
        producer_type: z.ZodString;
        producer_unwidened_type: z.ZodOptional<z.ZodString>;
        wire: z.ZodBoolean;
    }, "strip", z.ZodTypeAny, {
        wire: boolean;
        file_path: string;
        line_number: number;
        item_id: string;
        producer_type: string;
        span_start?: number | undefined;
        span_end?: number | undefined;
        expression_text?: string | undefined;
        expression_line?: number | undefined;
        producer_unwidened_type?: string | undefined;
    }, {
        wire: boolean;
        file_path: string;
        line_number: number;
        item_id: string;
        producer_type: string;
        span_start?: number | undefined;
        span_end?: number | undefined;
        expression_text?: string | undefined;
        expression_line?: number | undefined;
        producer_unwidened_type?: string | undefined;
    }>, "many">;
    budget_ms: z.ZodOptional<z.ZodNumber>;
}, "strip", z.ZodTypeAny, {
    request_id: string;
    action: "retype_check";
    items: {
        wire: boolean;
        file_path: string;
        line_number: number;
        item_id: string;
        producer_type: string;
        span_start?: number | undefined;
        span_end?: number | undefined;
        expression_text?: string | undefined;
        expression_line?: number | undefined;
        producer_unwidened_type?: string | undefined;
    }[];
    budget_ms?: number | undefined;
}, {
    request_id: string;
    action: "retype_check";
    items: {
        wire: boolean;
        file_path: string;
        line_number: number;
        item_id: string;
        producer_type: string;
        span_start?: number | undefined;
        span_end?: number | undefined;
        expression_text?: string | undefined;
        expression_line?: number | undefined;
        producer_unwidened_type?: string | undefined;
    }[];
    budget_ms?: number | undefined;
}>;
export declare const VerifyClientSemanticsRequestSchema: z.ZodObject<{
    request_id: z.ZodString;
} & {
    action: z.ZodLiteral<"verify_client_semantics">;
    from_dir: z.ZodString;
    checks: z.ZodArray<z.ZodObject<{
        claim_id: z.ZodString;
        package: z.ZodString;
        export: z.ZodString;
        receiver: z.ZodString;
        claim: z.ZodDiscriminatedUnion<"kind", [z.ZodObject<{
            kind: z.ZodLiteral<"factory">;
            member: z.ZodString;
            base_url_key: z.ZodString;
        }, "strip", z.ZodTypeAny, {
            kind: "factory";
            member: string;
            base_url_key: string;
        }, {
            kind: "factory";
            member: string;
            base_url_key: string;
        }>, z.ZodObject<{
            kind: z.ZodLiteral<"verb">;
            member: z.ZodString;
            method: z.ZodString;
        }, "strip", z.ZodTypeAny, {
            kind: "verb";
            member: string;
            method: string;
        }, {
            kind: "verb";
            member: string;
            method: string;
        }>, z.ZodObject<{
            kind: z.ZodLiteral<"verb_body">;
            member: z.ZodString;
            args: z.ZodEnum<["path_body", "path_options"]>;
            body_key: z.ZodOptional<z.ZodString>;
        }, "strip", z.ZodTypeAny, {
            kind: "verb_body";
            member: string;
            args: "path_body" | "path_options";
            body_key?: string | undefined;
        }, {
            kind: "verb_body";
            member: string;
            args: "path_body" | "path_options";
            body_key?: string | undefined;
        }>, z.ZodObject<{
            kind: z.ZodLiteral<"request">;
            member: z.ZodNullable<z.ZodString>;
            args: z.ZodEnum<["config", "path_options"]>;
            url_key: z.ZodOptional<z.ZodString>;
            method_key: z.ZodString;
        }, "strip", z.ZodTypeAny, {
            kind: "request";
            member: string | null;
            args: "path_options" | "config";
            method_key: string;
            url_key?: string | undefined;
        }, {
            kind: "request";
            member: string | null;
            args: "path_options" | "config";
            method_key: string;
            url_key?: string | undefined;
        }>, z.ZodObject<{
            kind: z.ZodLiteral<"request_body">;
            member: z.ZodNullable<z.ZodString>;
            args: z.ZodEnum<["config", "path_options"]>;
            url_key: z.ZodOptional<z.ZodString>;
            method_key: z.ZodString;
            body_key: z.ZodString;
        }, "strip", z.ZodTypeAny, {
            kind: "request_body";
            member: string | null;
            args: "path_options" | "config";
            body_key: string;
            method_key: string;
            url_key?: string | undefined;
        }, {
            kind: "request_body";
            member: string | null;
            args: "path_options" | "config";
            body_key: string;
            method_key: string;
            url_key?: string | undefined;
        }>]>;
    }, "strip", z.ZodTypeAny, {
        receiver: string;
        export: string;
        package: string;
        claim_id: string;
        claim: {
            kind: "factory";
            member: string;
            base_url_key: string;
        } | {
            kind: "verb";
            member: string;
            method: string;
        } | {
            kind: "verb_body";
            member: string;
            args: "path_body" | "path_options";
            body_key?: string | undefined;
        } | {
            kind: "request";
            member: string | null;
            args: "path_options" | "config";
            method_key: string;
            url_key?: string | undefined;
        } | {
            kind: "request_body";
            member: string | null;
            args: "path_options" | "config";
            body_key: string;
            method_key: string;
            url_key?: string | undefined;
        };
    }, {
        receiver: string;
        export: string;
        package: string;
        claim_id: string;
        claim: {
            kind: "factory";
            member: string;
            base_url_key: string;
        } | {
            kind: "verb";
            member: string;
            method: string;
        } | {
            kind: "verb_body";
            member: string;
            args: "path_body" | "path_options";
            body_key?: string | undefined;
        } | {
            kind: "request";
            member: string | null;
            args: "path_options" | "config";
            method_key: string;
            url_key?: string | undefined;
        } | {
            kind: "request_body";
            member: string | null;
            args: "path_options" | "config";
            body_key: string;
            method_key: string;
            url_key?: string | undefined;
        };
    }>, "many">;
    budget_ms: z.ZodOptional<z.ZodNumber>;
}, "strip", z.ZodTypeAny, {
    request_id: string;
    action: "verify_client_semantics";
    from_dir: string;
    checks: {
        receiver: string;
        export: string;
        package: string;
        claim_id: string;
        claim: {
            kind: "factory";
            member: string;
            base_url_key: string;
        } | {
            kind: "verb";
            member: string;
            method: string;
        } | {
            kind: "verb_body";
            member: string;
            args: "path_body" | "path_options";
            body_key?: string | undefined;
        } | {
            kind: "request";
            member: string | null;
            args: "path_options" | "config";
            method_key: string;
            url_key?: string | undefined;
        } | {
            kind: "request_body";
            member: string | null;
            args: "path_options" | "config";
            body_key: string;
            method_key: string;
            url_key?: string | undefined;
        };
    }[];
    budget_ms?: number | undefined;
}, {
    request_id: string;
    action: "verify_client_semantics";
    from_dir: string;
    checks: {
        receiver: string;
        export: string;
        package: string;
        claim_id: string;
        claim: {
            kind: "factory";
            member: string;
            base_url_key: string;
        } | {
            kind: "verb";
            member: string;
            method: string;
        } | {
            kind: "verb_body";
            member: string;
            args: "path_body" | "path_options";
            body_key?: string | undefined;
        } | {
            kind: "request";
            member: string | null;
            args: "path_options" | "config";
            method_key: string;
            url_key?: string | undefined;
        } | {
            kind: "request_body";
            member: string | null;
            args: "path_options" | "config";
            body_key: string;
            method_key: string;
            url_key?: string | undefined;
        };
    }[];
    budget_ms?: number | undefined;
}>;
/**
 * The checks of an `infer` request's server claims the library-check schema
 * takes, and how many it refused (carrick#2674). A refused claim reads
 * nothing; the others still do.
 */
export declare function settledServerChecks(checks: readonly unknown[]): {
    checks: LibraryCheck[];
    refused: number;
};
export declare const VerifyLibraryClaimsRequestSchema: z.ZodObject<{
    request_id: z.ZodString;
} & {
    action: z.ZodLiteral<"verify_library_claims">;
    from_dir: z.ZodString;
    scan_root: z.ZodOptional<z.ZodString>;
    checks: z.ZodArray<z.ZodObject<{
        claim_id: z.ZodString;
        package: z.ZodString;
        export: z.ZodString;
        role: z.ZodEnum<["http_client", "graphql_client", "broker", "in_process_bus", "socket", "server_framework", "invoke_client", "none"]>;
        receiver: z.ZodString;
        claim: z.ZodDiscriminatedUnion<"kind", [z.ZodObject<{
            kind: z.ZodLiteral<"make">;
            form: z.ZodEnum<["call", "new"]>;
            member: z.ZodNullable<z.ZodString>;
            base: z.ZodOptional<z.ZodObject<{
                arg: z.ZodNumber;
                key: z.ZodOptional<z.ZodString>;
                at: z.ZodOptional<z.ZodArray<z.ZodString, "many">>;
                each: z.ZodOptional<z.ZodBoolean>;
                handle: z.ZodOptional<z.ZodBoolean>;
                or_file: z.ZodOptional<z.ZodBoolean>;
            }, "strict", z.ZodTypeAny, {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            }, {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            }>>;
            prefix: z.ZodOptional<z.ZodObject<{
                arg: z.ZodNumber;
                key: z.ZodOptional<z.ZodString>;
                at: z.ZodOptional<z.ZodArray<z.ZodString, "many">>;
                each: z.ZodOptional<z.ZodBoolean>;
                handle: z.ZodOptional<z.ZodBoolean>;
                or_file: z.ZodOptional<z.ZodBoolean>;
            }, "strict", z.ZodTypeAny, {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            }, {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            }>>;
            name: z.ZodOptional<z.ZodObject<{
                arg: z.ZodNumber;
                key: z.ZodOptional<z.ZodString>;
                at: z.ZodOptional<z.ZodArray<z.ZodString, "many">>;
                each: z.ZodOptional<z.ZodBoolean>;
                handle: z.ZodOptional<z.ZodBoolean>;
                or_file: z.ZodOptional<z.ZodBoolean>;
            } & {
                document: z.ZodOptional<z.ZodLiteral<"graphql">>;
                ref: z.ZodOptional<z.ZodEnum<["function", "class"]>>;
            }, "strict", z.ZodTypeAny, {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                document?: "graphql" | undefined;
                ref?: "function" | "class" | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            }, {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                document?: "graphql" | undefined;
                ref?: "function" | "class" | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            }>>;
            handler: z.ZodOptional<z.ZodObject<{
                arg: z.ZodNumber;
                key: z.ZodOptional<z.ZodString>;
                at: z.ZodOptional<z.ZodArray<z.ZodString, "many">>;
                each: z.ZodOptional<z.ZodBoolean>;
                handle: z.ZodOptional<z.ZodBoolean>;
                or_file: z.ZodOptional<z.ZodBoolean>;
            }, "strict", z.ZodTypeAny, {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            }, {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            }>>;
            runs: z.ZodOptional<z.ZodObject<{
                arg: z.ZodNumber;
                key: z.ZodOptional<z.ZodString>;
                at: z.ZodOptional<z.ZodArray<z.ZodString, "many">>;
                each: z.ZodOptional<z.ZodBoolean>;
                handle: z.ZodOptional<z.ZodBoolean>;
                or_file: z.ZodOptional<z.ZodBoolean>;
            }, "strict", z.ZodTypeAny, {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            }, {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            }>>;
            key_labels: z.ZodOptional<z.ZodRecord<z.ZodString, z.ZodEnum<["name", "not_name"]>>>;
            name_scope: z.ZodOptional<z.ZodObject<{
                scope: z.ZodEnum<["global", "service"]>;
                namespace: z.ZodNullable<z.ZodString>;
            }, "strict", z.ZodTypeAny, {
                scope: "global" | "service";
                namespace: string | null;
            }, {
                scope: "global" | "service";
                namespace: string | null;
            }>>;
            picker: z.ZodOptional<z.ZodString>;
            result: z.ZodOptional<z.ZodUnion<[z.ZodObject<{
                whole: z.ZodLiteral<true>;
            }, "strict", z.ZodTypeAny, {
                whole: true;
            }, {
                whole: true;
            }>, z.ZodObject<{
                member: z.ZodTuple<[z.ZodString], null>;
            }, "strict", z.ZodTypeAny, {
                member: [string];
            }, {
                member: [string];
            }>, z.ZodObject<{
                parse: z.ZodString;
            }, "strict", z.ZodTypeAny, {
                parse: string;
            }, {
                parse: string;
            }>]>>;
        }, "strict", z.ZodTypeAny, {
            kind: "make";
            member: string | null;
            form: "call" | "new";
            name?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                document?: "graphql" | undefined;
                ref?: "function" | "class" | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            result?: {
                whole: true;
            } | {
                member: [string];
            } | {
                parse: string;
            } | undefined;
            base?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            runs?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            prefix?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            handler?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            key_labels?: Record<string, "name" | "not_name"> | undefined;
            name_scope?: {
                scope: "global" | "service";
                namespace: string | null;
            } | undefined;
            picker?: string | undefined;
        }, {
            kind: "make";
            member: string | null;
            form: "call" | "new";
            name?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                document?: "graphql" | undefined;
                ref?: "function" | "class" | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            result?: {
                whole: true;
            } | {
                member: [string];
            } | {
                parse: string;
            } | undefined;
            base?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            runs?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            prefix?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            handler?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            key_labels?: Record<string, "name" | "not_name"> | undefined;
            name_scope?: {
                scope: "global" | "service";
                namespace: string | null;
            } | undefined;
            picker?: string | undefined;
        }>, z.ZodObject<{
            kind: z.ZodLiteral<"scope">;
            member: z.ZodString;
            form: z.ZodOptional<z.ZodEnum<["call", "decorate"]>>;
            name: z.ZodOptional<z.ZodObject<{
                arg: z.ZodNumber;
                key: z.ZodOptional<z.ZodString>;
                at: z.ZodOptional<z.ZodArray<z.ZodString, "many">>;
                each: z.ZodOptional<z.ZodBoolean>;
                handle: z.ZodOptional<z.ZodBoolean>;
                or_file: z.ZodOptional<z.ZodBoolean>;
            } & {
                document: z.ZodOptional<z.ZodLiteral<"graphql">>;
                ref: z.ZodOptional<z.ZodEnum<["function", "class"]>>;
            }, "strict", z.ZodTypeAny, {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                document?: "graphql" | undefined;
                ref?: "function" | "class" | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            }, {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                document?: "graphql" | undefined;
                ref?: "function" | "class" | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            }>>;
            path: z.ZodOptional<z.ZodArray<z.ZodString, "many">>;
            on: z.ZodOptional<z.ZodEnum<["export", "instance", "both"]>>;
            of: z.ZodOptional<z.ZodString>;
            key_labels: z.ZodOptional<z.ZodRecord<z.ZodString, z.ZodEnum<["name", "not_name"]>>>;
            name_scope: z.ZodOptional<z.ZodObject<{
                scope: z.ZodEnum<["global", "service"]>;
                namespace: z.ZodNullable<z.ZodString>;
            }, "strict", z.ZodTypeAny, {
                scope: "global" | "service";
                namespace: string | null;
            }, {
                scope: "global" | "service";
                namespace: string | null;
            }>>;
            picker: z.ZodOptional<z.ZodString>;
            runs: z.ZodOptional<z.ZodObject<{
                arg: z.ZodNumber;
                key: z.ZodOptional<z.ZodString>;
                at: z.ZodOptional<z.ZodArray<z.ZodString, "many">>;
                each: z.ZodOptional<z.ZodBoolean>;
                handle: z.ZodOptional<z.ZodBoolean>;
                or_file: z.ZodOptional<z.ZodBoolean>;
            }, "strict", z.ZodTypeAny, {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            }, {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            }>>;
            yields: z.ZodOptional<z.ZodObject<{
                param: z.ZodNumber;
                export: z.ZodString;
                side: z.ZodString;
                package: z.ZodOptional<z.ZodString>;
            }, "strict", z.ZodTypeAny, {
                side: string;
                export: string;
                param: number;
                package?: string | undefined;
            }, {
                side: string;
                export: string;
                param: number;
                package?: string | undefined;
            }>>;
            mounts: z.ZodOptional<z.ZodObject<{
                arg: z.ZodNumber;
                key: z.ZodOptional<z.ZodString>;
                at: z.ZodOptional<z.ZodArray<z.ZodString, "many">>;
                each: z.ZodOptional<z.ZodBoolean>;
                handle: z.ZodOptional<z.ZodBoolean>;
                or_file: z.ZodOptional<z.ZodBoolean>;
            }, "strict", z.ZodTypeAny, {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            }, {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            }>>;
            collects: z.ZodOptional<z.ZodObject<{
                arg: z.ZodNumber;
                key: z.ZodOptional<z.ZodString>;
                join: z.ZodString;
            }, "strict", z.ZodTypeAny, {
                join: string;
                arg: number;
                key?: string | undefined;
            }, {
                join: string;
                arg: number;
                key?: string | undefined;
            }>>;
            merges: z.ZodOptional<z.ZodObject<{
                arg: z.ZodNumber;
                key: z.ZodOptional<z.ZodString>;
                at: z.ZodOptional<z.ZodArray<z.ZodString, "many">>;
                each: z.ZodOptional<z.ZodBoolean>;
                handle: z.ZodOptional<z.ZodBoolean>;
                or_file: z.ZodOptional<z.ZodBoolean>;
            }, "strict", z.ZodTypeAny, {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            }, {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            }>>;
            passes: z.ZodOptional<z.ZodBoolean>;
        }, "strict", z.ZodTypeAny, {
            kind: "scope";
            member: string;
            name?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                document?: "graphql" | undefined;
                ref?: "function" | "class" | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            path?: string[] | undefined;
            runs?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            yields?: {
                side: string;
                export: string;
                param: number;
                package?: string | undefined;
            } | undefined;
            form?: "call" | "decorate" | undefined;
            key_labels?: Record<string, "name" | "not_name"> | undefined;
            name_scope?: {
                scope: "global" | "service";
                namespace: string | null;
            } | undefined;
            of?: string | undefined;
            on?: "both" | "export" | "instance" | undefined;
            mounts?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            merges?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            picker?: string | undefined;
            collects?: {
                join: string;
                arg: number;
                key?: string | undefined;
            } | undefined;
            passes?: boolean | undefined;
        }, {
            kind: "scope";
            member: string;
            name?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                document?: "graphql" | undefined;
                ref?: "function" | "class" | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            path?: string[] | undefined;
            runs?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            yields?: {
                side: string;
                export: string;
                param: number;
                package?: string | undefined;
            } | undefined;
            form?: "call" | "decorate" | undefined;
            key_labels?: Record<string, "name" | "not_name"> | undefined;
            name_scope?: {
                scope: "global" | "service";
                namespace: string | null;
            } | undefined;
            of?: string | undefined;
            on?: "both" | "export" | "instance" | undefined;
            mounts?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            merges?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            picker?: string | undefined;
            collects?: {
                join: string;
                arg: number;
                key?: string | undefined;
            } | undefined;
            passes?: boolean | undefined;
        }>, z.ZodObject<{
            kind: z.ZodLiteral<"op">;
            op: z.ZodEnum<["request", "send", "receive", "execute"]>;
            member: z.ZodNullable<z.ZodString>;
            path: z.ZodOptional<z.ZodArray<z.ZodString, "many">>;
            on: z.ZodOptional<z.ZodEnum<["export", "instance", "both"]>>;
            of: z.ZodOptional<z.ZodString>;
            form: z.ZodOptional<z.ZodEnum<["call", "new", "set", "override", "decorate"]>>;
            protocol: z.ZodOptional<z.ZodEnum<["http", "graphql", "rpc", "socket", "message"]>>;
            when: z.ZodOptional<z.ZodObject<{
                arg: z.ZodNumber;
                key: z.ZodOptional<z.ZodString>;
                literal: z.ZodString;
            }, "strict", z.ZodTypeAny, {
                literal: string;
                arg: number;
                key?: string | undefined;
            }, {
                literal: string;
                arg: number;
                key?: string | undefined;
            }>>;
            returns: z.ZodOptional<z.ZodObject<{
                result: z.ZodOptional<z.ZodUnion<[z.ZodObject<{
                    whole: z.ZodLiteral<true>;
                }, "strict", z.ZodTypeAny, {
                    whole: true;
                }, {
                    whole: true;
                }>, z.ZodObject<{
                    member: z.ZodTuple<[z.ZodString], null>;
                }, "strict", z.ZodTypeAny, {
                    member: [string];
                }, {
                    member: [string];
                }>, z.ZodObject<{
                    parse: z.ZodString;
                }, "strict", z.ZodTypeAny, {
                    parse: string;
                }, {
                    parse: string;
                }>]>>;
            }, "strict", z.ZodTypeAny, {
                result?: {
                    whole: true;
                } | {
                    member: [string];
                } | {
                    parse: string;
                } | undefined;
            }, {
                result?: {
                    whole: true;
                } | {
                    member: [string];
                } | {
                    parse: string;
                } | undefined;
            }>>;
            name: z.ZodOptional<z.ZodUnion<[z.ZodObject<{
                bound: z.ZodEnum<["maker", "scope", "key", "path", "export", "subscribed", "decorated"]>;
                join: z.ZodOptional<z.ZodString>;
                param: z.ZodOptional<z.ZodNumber>;
                at: z.ZodOptional<z.ZodArray<z.ZodString, "many">>;
            }, "strict", z.ZodTypeAny, {
                bound: "decorated" | "maker" | "scope" | "key" | "path" | "export" | "subscribed";
                join?: string | undefined;
                at?: string[] | undefined;
                param?: number | undefined;
            }, {
                bound: "decorated" | "maker" | "scope" | "key" | "path" | "export" | "subscribed";
                join?: string | undefined;
                at?: string[] | undefined;
                param?: number | undefined;
            }>, z.ZodObject<{
                parts: z.ZodArray<z.ZodObject<{
                    arg: z.ZodNumber;
                    key: z.ZodOptional<z.ZodString>;
                    at: z.ZodOptional<z.ZodArray<z.ZodString, "many">>;
                    each: z.ZodOptional<z.ZodBoolean>;
                    handle: z.ZodOptional<z.ZodBoolean>;
                    or_file: z.ZodOptional<z.ZodBoolean>;
                }, "strict", z.ZodTypeAny, {
                    arg: number;
                    at?: string[] | undefined;
                    key?: string | undefined;
                    each?: boolean | undefined;
                    handle?: boolean | undefined;
                    or_file?: boolean | undefined;
                }, {
                    arg: number;
                    at?: string[] | undefined;
                    key?: string | undefined;
                    each?: boolean | undefined;
                    handle?: boolean | undefined;
                    or_file?: boolean | undefined;
                }>, "many">;
                join: z.ZodOptional<z.ZodString>;
            }, "strict", z.ZodTypeAny, {
                parts: {
                    arg: number;
                    at?: string[] | undefined;
                    key?: string | undefined;
                    each?: boolean | undefined;
                    handle?: boolean | undefined;
                    or_file?: boolean | undefined;
                }[];
                join?: string | undefined;
            }, {
                parts: {
                    arg: number;
                    at?: string[] | undefined;
                    key?: string | undefined;
                    each?: boolean | undefined;
                    handle?: boolean | undefined;
                    or_file?: boolean | undefined;
                }[];
                join?: string | undefined;
            }>, z.ZodObject<{
                literal: z.ZodString;
            }, "strict", z.ZodTypeAny, {
                literal: string;
            }, {
                literal: string;
            }>, z.ZodObject<{
                first: z.ZodArray<z.ZodUnion<[z.ZodObject<{
                    arg: z.ZodNumber;
                    key: z.ZodOptional<z.ZodString>;
                }, "strict", z.ZodTypeAny, {
                    arg: number;
                    key?: string | undefined;
                }, {
                    arg: number;
                    key?: string | undefined;
                }>, z.ZodObject<{
                    bound: z.ZodLiteral<"decorated">;
                }, "strict", z.ZodTypeAny, {
                    bound: "decorated";
                }, {
                    bound: "decorated";
                }>]>, "many">;
            }, "strict", z.ZodTypeAny, {
                first: ({
                    arg: number;
                    key?: string | undefined;
                } | {
                    bound: "decorated";
                })[];
            }, {
                first: ({
                    arg: number;
                    key?: string | undefined;
                } | {
                    bound: "decorated";
                })[];
            }>, z.ZodObject<{
                arg: z.ZodNumber;
                key: z.ZodOptional<z.ZodString>;
                at: z.ZodOptional<z.ZodArray<z.ZodString, "many">>;
                each: z.ZodOptional<z.ZodBoolean>;
                handle: z.ZodOptional<z.ZodBoolean>;
                or_file: z.ZodOptional<z.ZodBoolean>;
            } & {
                document: z.ZodOptional<z.ZodLiteral<"graphql">>;
                ref: z.ZodOptional<z.ZodEnum<["function", "class"]>>;
            }, "strict", z.ZodTypeAny, {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                document?: "graphql" | undefined;
                ref?: "function" | "class" | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            }, {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                document?: "graphql" | undefined;
                ref?: "function" | "class" | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            }>]>>;
            payload: z.ZodOptional<z.ZodObject<{
                arg: z.ZodNumber;
                key: z.ZodOptional<z.ZodString>;
                at: z.ZodOptional<z.ZodArray<z.ZodString, "many">>;
                each: z.ZodOptional<z.ZodBoolean>;
                handle: z.ZodOptional<z.ZodBoolean>;
                or_file: z.ZodOptional<z.ZodBoolean>;
            }, "strict", z.ZodTypeAny, {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            }, {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            }>>;
            handler: z.ZodOptional<z.ZodObject<{
                arg: z.ZodNumber;
                key: z.ZodOptional<z.ZodString>;
                at: z.ZodOptional<z.ZodArray<z.ZodString, "many">>;
                each: z.ZodOptional<z.ZodBoolean>;
                handle: z.ZodOptional<z.ZodBoolean>;
                or_file: z.ZodOptional<z.ZodBoolean>;
            }, "strict", z.ZodTypeAny, {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            }, {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            }>>;
            ack: z.ZodOptional<z.ZodObject<{
                arg: z.ZodNumber;
                key: z.ZodOptional<z.ZodString>;
                at: z.ZodOptional<z.ZodArray<z.ZodString, "many">>;
                each: z.ZodOptional<z.ZodBoolean>;
                handle: z.ZodOptional<z.ZodBoolean>;
                or_file: z.ZodOptional<z.ZodBoolean>;
            }, "strict", z.ZodTypeAny, {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            }, {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            }>>;
            key_labels: z.ZodOptional<z.ZodRecord<z.ZodString, z.ZodEnum<["name", "not_name"]>>>;
            name_scope: z.ZodOptional<z.ZodObject<{
                scope: z.ZodEnum<["global", "service"]>;
                namespace: z.ZodNullable<z.ZodString>;
            }, "strict", z.ZodTypeAny, {
                scope: "global" | "service";
                namespace: string | null;
            }, {
                scope: "global" | "service";
                namespace: string | null;
            }>>;
            picker: z.ZodOptional<z.ZodString>;
            options: z.ZodOptional<z.ZodObject<{
                arg: z.ZodNumber;
                key: z.ZodOptional<z.ZodString>;
                at: z.ZodOptional<z.ZodArray<z.ZodString, "many">>;
                each: z.ZodOptional<z.ZodBoolean>;
                handle: z.ZodOptional<z.ZodBoolean>;
                or_file: z.ZodOptional<z.ZodBoolean>;
            }, "strict", z.ZodTypeAny, {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            }, {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            }>>;
            method: z.ZodOptional<z.ZodString>;
            method_key: z.ZodOptional<z.ZodObject<{
                arg: z.ZodNumber;
                key: z.ZodOptional<z.ZodString>;
                at: z.ZodOptional<z.ZodArray<z.ZodString, "many">>;
                each: z.ZodOptional<z.ZodBoolean>;
                handle: z.ZodOptional<z.ZodBoolean>;
                or_file: z.ZodOptional<z.ZodBoolean>;
            }, "strict", z.ZodTypeAny, {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            }, {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            }>>;
            method_default: z.ZodOptional<z.ZodString>;
            base: z.ZodOptional<z.ZodObject<{
                arg: z.ZodNumber;
                key: z.ZodOptional<z.ZodString>;
                at: z.ZodOptional<z.ZodArray<z.ZodString, "many">>;
                each: z.ZodOptional<z.ZodBoolean>;
                handle: z.ZodOptional<z.ZodBoolean>;
                or_file: z.ZodOptional<z.ZodBoolean>;
            }, "strict", z.ZodTypeAny, {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            }, {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            }>>;
            runs: z.ZodOptional<z.ZodObject<{
                arg: z.ZodNumber;
                key: z.ZodOptional<z.ZodString>;
                bound: z.ZodOptional<z.ZodLiteral<"maker">>;
            }, "strict", z.ZodTypeAny, {
                arg: number;
                key?: string | undefined;
                bound?: "maker" | undefined;
            }, {
                arg: number;
                key?: string | undefined;
                bound?: "maker" | undefined;
            }>>;
            yields: z.ZodOptional<z.ZodObject<{
                param: z.ZodNumber;
                export: z.ZodString;
                side: z.ZodString;
                package: z.ZodOptional<z.ZodString>;
            }, "strict", z.ZodTypeAny, {
                side: string;
                export: string;
                param: number;
                package?: string | undefined;
            }, {
                side: string;
                export: string;
                param: number;
                package?: string | undefined;
            }>>;
            request: z.ZodOptional<z.ZodObject<{
                param: z.ZodNumber;
                path: z.ZodOptional<z.ZodArray<z.ZodString, "many">>;
                url: z.ZodOptional<z.ZodArray<z.ZodString, "many">>;
                method: z.ZodOptional<z.ZodArray<z.ZodString, "many">>;
                body: z.ZodOptional<z.ZodArray<z.ZodString, "many">>;
            }, "strict", z.ZodTypeAny, {
                param: number;
                body?: string[] | undefined;
                url?: string[] | undefined;
                path?: string[] | undefined;
                method?: string[] | undefined;
            }, {
                param: number;
                body?: string[] | undefined;
                url?: string[] | undefined;
                path?: string[] | undefined;
                method?: string[] | undefined;
            }>>;
            result: z.ZodOptional<z.ZodUnion<[z.ZodObject<{
                whole: z.ZodLiteral<true>;
            }, "strict", z.ZodTypeAny, {
                whole: true;
            }, {
                whole: true;
            }>, z.ZodObject<{
                member: z.ZodTuple<[z.ZodString], null>;
            }, "strict", z.ZodTypeAny, {
                member: [string];
            }, {
                member: [string];
            }>, z.ZodObject<{
                parse: z.ZodString;
            }, "strict", z.ZodTypeAny, {
                parse: string;
            }, {
                parse: string;
            }>]>>;
        }, "strict", z.ZodTypeAny, {
            kind: "op";
            member: string | null;
            op: "request" | "send" | "receive" | "execute";
            name?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                document?: "graphql" | undefined;
                ref?: "function" | "class" | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | {
                parts: {
                    arg: number;
                    at?: string[] | undefined;
                    key?: string | undefined;
                    each?: boolean | undefined;
                    handle?: boolean | undefined;
                    or_file?: boolean | undefined;
                }[];
                join?: string | undefined;
            } | {
                literal: string;
            } | {
                bound: "decorated" | "maker" | "scope" | "key" | "path" | "export" | "subscribed";
                join?: string | undefined;
                at?: string[] | undefined;
                param?: number | undefined;
            } | {
                first: ({
                    arg: number;
                    key?: string | undefined;
                } | {
                    bound: "decorated";
                })[];
            } | undefined;
            request?: {
                param: number;
                body?: string[] | undefined;
                url?: string[] | undefined;
                path?: string[] | undefined;
                method?: string[] | undefined;
            } | undefined;
            path?: string[] | undefined;
            result?: {
                whole: true;
            } | {
                member: [string];
            } | {
                parse: string;
            } | undefined;
            method_default?: string | undefined;
            base?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            runs?: {
                arg: number;
                key?: string | undefined;
                bound?: "maker" | undefined;
            } | undefined;
            yields?: {
                side: string;
                export: string;
                param: number;
                package?: string | undefined;
            } | undefined;
            form?: "call" | "new" | "set" | "override" | "decorate" | undefined;
            handler?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            key_labels?: Record<string, "name" | "not_name"> | undefined;
            name_scope?: {
                scope: "global" | "service";
                namespace: string | null;
            } | undefined;
            method?: string | undefined;
            payload?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            options?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            method_key?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            returns?: {
                result?: {
                    whole: true;
                } | {
                    member: [string];
                } | {
                    parse: string;
                } | undefined;
            } | undefined;
            when?: {
                literal: string;
                arg: number;
                key?: string | undefined;
            } | undefined;
            of?: string | undefined;
            on?: "both" | "export" | "instance" | undefined;
            ack?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            protocol?: "http" | "graphql" | "socket" | "rpc" | "message" | undefined;
            picker?: string | undefined;
        }, {
            kind: "op";
            member: string | null;
            op: "request" | "send" | "receive" | "execute";
            name?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                document?: "graphql" | undefined;
                ref?: "function" | "class" | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | {
                parts: {
                    arg: number;
                    at?: string[] | undefined;
                    key?: string | undefined;
                    each?: boolean | undefined;
                    handle?: boolean | undefined;
                    or_file?: boolean | undefined;
                }[];
                join?: string | undefined;
            } | {
                literal: string;
            } | {
                bound: "decorated" | "maker" | "scope" | "key" | "path" | "export" | "subscribed";
                join?: string | undefined;
                at?: string[] | undefined;
                param?: number | undefined;
            } | {
                first: ({
                    arg: number;
                    key?: string | undefined;
                } | {
                    bound: "decorated";
                })[];
            } | undefined;
            request?: {
                param: number;
                body?: string[] | undefined;
                url?: string[] | undefined;
                path?: string[] | undefined;
                method?: string[] | undefined;
            } | undefined;
            path?: string[] | undefined;
            result?: {
                whole: true;
            } | {
                member: [string];
            } | {
                parse: string;
            } | undefined;
            method_default?: string | undefined;
            base?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            runs?: {
                arg: number;
                key?: string | undefined;
                bound?: "maker" | undefined;
            } | undefined;
            yields?: {
                side: string;
                export: string;
                param: number;
                package?: string | undefined;
            } | undefined;
            form?: "call" | "new" | "set" | "override" | "decorate" | undefined;
            handler?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            key_labels?: Record<string, "name" | "not_name"> | undefined;
            name_scope?: {
                scope: "global" | "service";
                namespace: string | null;
            } | undefined;
            method?: string | undefined;
            payload?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            options?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            method_key?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            returns?: {
                result?: {
                    whole: true;
                } | {
                    member: [string];
                } | {
                    parse: string;
                } | undefined;
            } | undefined;
            when?: {
                literal: string;
                arg: number;
                key?: string | undefined;
            } | undefined;
            of?: string | undefined;
            on?: "both" | "export" | "instance" | undefined;
            ack?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            protocol?: "http" | "graphql" | "socket" | "rpc" | "message" | undefined;
            picker?: string | undefined;
        }>, z.ZodObject<{
            kind: z.ZodLiteral<"result">;
            form: z.ZodEnum<["make", "op"]>;
            member: z.ZodNullable<z.ZodString>;
            path: z.ZodOptional<z.ZodArray<z.ZodString, "many">>;
            on: z.ZodOptional<z.ZodEnum<["export", "instance", "both"]>>;
            of: z.ZodOptional<z.ZodString>;
            picker: z.ZodOptional<z.ZodString>;
            result: z.ZodUnion<[z.ZodObject<{
                whole: z.ZodLiteral<true>;
            }, "strict", z.ZodTypeAny, {
                whole: true;
            }, {
                whole: true;
            }>, z.ZodObject<{
                member: z.ZodTuple<[z.ZodString], null>;
            }, "strict", z.ZodTypeAny, {
                member: [string];
            }, {
                member: [string];
            }>, z.ZodObject<{
                parse: z.ZodString;
            }, "strict", z.ZodTypeAny, {
                parse: string;
            }, {
                parse: string;
            }>]>;
        }, "strict", z.ZodTypeAny, {
            kind: "result";
            member: string | null;
            result: {
                whole: true;
            } | {
                member: [string];
            } | {
                parse: string;
            };
            form: "make" | "op";
            path?: string[] | undefined;
            of?: string | undefined;
            on?: "both" | "export" | "instance" | undefined;
            picker?: string | undefined;
        }, {
            kind: "result";
            member: string | null;
            result: {
                whole: true;
            } | {
                member: [string];
            } | {
                parse: string;
            };
            form: "make" | "op";
            path?: string[] | undefined;
            of?: string | undefined;
            on?: "both" | "export" | "instance" | undefined;
            picker?: string | undefined;
        }>, z.ZodObject<{
            kind: z.ZodLiteral<"reserved">;
            member: z.ZodString;
            name: z.ZodString;
            path: z.ZodOptional<z.ZodArray<z.ZodString, "many">>;
            on: z.ZodOptional<z.ZodEnum<["export", "instance", "both"]>>;
            of: z.ZodOptional<z.ZodString>;
            picker: z.ZodOptional<z.ZodString>;
        }, "strict", z.ZodTypeAny, {
            kind: "reserved";
            name: string;
            member: string;
            path?: string[] | undefined;
            of?: string | undefined;
            on?: "both" | "export" | "instance" | undefined;
            picker?: string | undefined;
        }, {
            kind: "reserved";
            name: string;
            member: string;
            path?: string[] | undefined;
            of?: string | undefined;
            on?: "both" | "export" | "instance" | undefined;
            picker?: string | undefined;
        }>]>;
    }, "strict", z.ZodTypeAny, {
        receiver: string;
        export: string;
        package: string;
        claim_id: string;
        claim: {
            kind: "make";
            member: string | null;
            form: "call" | "new";
            name?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                document?: "graphql" | undefined;
                ref?: "function" | "class" | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            result?: {
                whole: true;
            } | {
                member: [string];
            } | {
                parse: string;
            } | undefined;
            base?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            runs?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            prefix?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            handler?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            key_labels?: Record<string, "name" | "not_name"> | undefined;
            name_scope?: {
                scope: "global" | "service";
                namespace: string | null;
            } | undefined;
            picker?: string | undefined;
        } | {
            kind: "scope";
            member: string;
            name?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                document?: "graphql" | undefined;
                ref?: "function" | "class" | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            path?: string[] | undefined;
            runs?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            yields?: {
                side: string;
                export: string;
                param: number;
                package?: string | undefined;
            } | undefined;
            form?: "call" | "decorate" | undefined;
            key_labels?: Record<string, "name" | "not_name"> | undefined;
            name_scope?: {
                scope: "global" | "service";
                namespace: string | null;
            } | undefined;
            of?: string | undefined;
            on?: "both" | "export" | "instance" | undefined;
            mounts?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            merges?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            picker?: string | undefined;
            collects?: {
                join: string;
                arg: number;
                key?: string | undefined;
            } | undefined;
            passes?: boolean | undefined;
        } | {
            kind: "op";
            member: string | null;
            op: "request" | "send" | "receive" | "execute";
            name?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                document?: "graphql" | undefined;
                ref?: "function" | "class" | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | {
                parts: {
                    arg: number;
                    at?: string[] | undefined;
                    key?: string | undefined;
                    each?: boolean | undefined;
                    handle?: boolean | undefined;
                    or_file?: boolean | undefined;
                }[];
                join?: string | undefined;
            } | {
                literal: string;
            } | {
                bound: "decorated" | "maker" | "scope" | "key" | "path" | "export" | "subscribed";
                join?: string | undefined;
                at?: string[] | undefined;
                param?: number | undefined;
            } | {
                first: ({
                    arg: number;
                    key?: string | undefined;
                } | {
                    bound: "decorated";
                })[];
            } | undefined;
            request?: {
                param: number;
                body?: string[] | undefined;
                url?: string[] | undefined;
                path?: string[] | undefined;
                method?: string[] | undefined;
            } | undefined;
            path?: string[] | undefined;
            result?: {
                whole: true;
            } | {
                member: [string];
            } | {
                parse: string;
            } | undefined;
            method_default?: string | undefined;
            base?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            runs?: {
                arg: number;
                key?: string | undefined;
                bound?: "maker" | undefined;
            } | undefined;
            yields?: {
                side: string;
                export: string;
                param: number;
                package?: string | undefined;
            } | undefined;
            form?: "call" | "new" | "set" | "override" | "decorate" | undefined;
            handler?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            key_labels?: Record<string, "name" | "not_name"> | undefined;
            name_scope?: {
                scope: "global" | "service";
                namespace: string | null;
            } | undefined;
            method?: string | undefined;
            payload?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            options?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            method_key?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            returns?: {
                result?: {
                    whole: true;
                } | {
                    member: [string];
                } | {
                    parse: string;
                } | undefined;
            } | undefined;
            when?: {
                literal: string;
                arg: number;
                key?: string | undefined;
            } | undefined;
            of?: string | undefined;
            on?: "both" | "export" | "instance" | undefined;
            ack?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            protocol?: "http" | "graphql" | "socket" | "rpc" | "message" | undefined;
            picker?: string | undefined;
        } | {
            kind: "result";
            member: string | null;
            result: {
                whole: true;
            } | {
                member: [string];
            } | {
                parse: string;
            };
            form: "make" | "op";
            path?: string[] | undefined;
            of?: string | undefined;
            on?: "both" | "export" | "instance" | undefined;
            picker?: string | undefined;
        } | {
            kind: "reserved";
            name: string;
            member: string;
            path?: string[] | undefined;
            of?: string | undefined;
            on?: "both" | "export" | "instance" | undefined;
            picker?: string | undefined;
        };
        role: "none" | "socket" | "http_client" | "graphql_client" | "broker" | "in_process_bus" | "server_framework" | "invoke_client";
    }, {
        receiver: string;
        export: string;
        package: string;
        claim_id: string;
        claim: {
            kind: "make";
            member: string | null;
            form: "call" | "new";
            name?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                document?: "graphql" | undefined;
                ref?: "function" | "class" | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            result?: {
                whole: true;
            } | {
                member: [string];
            } | {
                parse: string;
            } | undefined;
            base?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            runs?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            prefix?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            handler?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            key_labels?: Record<string, "name" | "not_name"> | undefined;
            name_scope?: {
                scope: "global" | "service";
                namespace: string | null;
            } | undefined;
            picker?: string | undefined;
        } | {
            kind: "scope";
            member: string;
            name?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                document?: "graphql" | undefined;
                ref?: "function" | "class" | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            path?: string[] | undefined;
            runs?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            yields?: {
                side: string;
                export: string;
                param: number;
                package?: string | undefined;
            } | undefined;
            form?: "call" | "decorate" | undefined;
            key_labels?: Record<string, "name" | "not_name"> | undefined;
            name_scope?: {
                scope: "global" | "service";
                namespace: string | null;
            } | undefined;
            of?: string | undefined;
            on?: "both" | "export" | "instance" | undefined;
            mounts?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            merges?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            picker?: string | undefined;
            collects?: {
                join: string;
                arg: number;
                key?: string | undefined;
            } | undefined;
            passes?: boolean | undefined;
        } | {
            kind: "op";
            member: string | null;
            op: "request" | "send" | "receive" | "execute";
            name?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                document?: "graphql" | undefined;
                ref?: "function" | "class" | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | {
                parts: {
                    arg: number;
                    at?: string[] | undefined;
                    key?: string | undefined;
                    each?: boolean | undefined;
                    handle?: boolean | undefined;
                    or_file?: boolean | undefined;
                }[];
                join?: string | undefined;
            } | {
                literal: string;
            } | {
                bound: "decorated" | "maker" | "scope" | "key" | "path" | "export" | "subscribed";
                join?: string | undefined;
                at?: string[] | undefined;
                param?: number | undefined;
            } | {
                first: ({
                    arg: number;
                    key?: string | undefined;
                } | {
                    bound: "decorated";
                })[];
            } | undefined;
            request?: {
                param: number;
                body?: string[] | undefined;
                url?: string[] | undefined;
                path?: string[] | undefined;
                method?: string[] | undefined;
            } | undefined;
            path?: string[] | undefined;
            result?: {
                whole: true;
            } | {
                member: [string];
            } | {
                parse: string;
            } | undefined;
            method_default?: string | undefined;
            base?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            runs?: {
                arg: number;
                key?: string | undefined;
                bound?: "maker" | undefined;
            } | undefined;
            yields?: {
                side: string;
                export: string;
                param: number;
                package?: string | undefined;
            } | undefined;
            form?: "call" | "new" | "set" | "override" | "decorate" | undefined;
            handler?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            key_labels?: Record<string, "name" | "not_name"> | undefined;
            name_scope?: {
                scope: "global" | "service";
                namespace: string | null;
            } | undefined;
            method?: string | undefined;
            payload?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            options?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            method_key?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            returns?: {
                result?: {
                    whole: true;
                } | {
                    member: [string];
                } | {
                    parse: string;
                } | undefined;
            } | undefined;
            when?: {
                literal: string;
                arg: number;
                key?: string | undefined;
            } | undefined;
            of?: string | undefined;
            on?: "both" | "export" | "instance" | undefined;
            ack?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            protocol?: "http" | "graphql" | "socket" | "rpc" | "message" | undefined;
            picker?: string | undefined;
        } | {
            kind: "result";
            member: string | null;
            result: {
                whole: true;
            } | {
                member: [string];
            } | {
                parse: string;
            };
            form: "make" | "op";
            path?: string[] | undefined;
            of?: string | undefined;
            on?: "both" | "export" | "instance" | undefined;
            picker?: string | undefined;
        } | {
            kind: "reserved";
            name: string;
            member: string;
            path?: string[] | undefined;
            of?: string | undefined;
            on?: "both" | "export" | "instance" | undefined;
            picker?: string | undefined;
        };
        role: "none" | "socket" | "http_client" | "graphql_client" | "broker" | "in_process_bus" | "server_framework" | "invoke_client";
    }>, "many">;
    budget_ms: z.ZodOptional<z.ZodNullable<z.ZodNumber>>;
}, "strip", z.ZodTypeAny, {
    request_id: string;
    action: "verify_library_claims";
    from_dir: string;
    checks: {
        receiver: string;
        export: string;
        package: string;
        claim_id: string;
        claim: {
            kind: "make";
            member: string | null;
            form: "call" | "new";
            name?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                document?: "graphql" | undefined;
                ref?: "function" | "class" | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            result?: {
                whole: true;
            } | {
                member: [string];
            } | {
                parse: string;
            } | undefined;
            base?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            runs?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            prefix?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            handler?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            key_labels?: Record<string, "name" | "not_name"> | undefined;
            name_scope?: {
                scope: "global" | "service";
                namespace: string | null;
            } | undefined;
            picker?: string | undefined;
        } | {
            kind: "scope";
            member: string;
            name?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                document?: "graphql" | undefined;
                ref?: "function" | "class" | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            path?: string[] | undefined;
            runs?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            yields?: {
                side: string;
                export: string;
                param: number;
                package?: string | undefined;
            } | undefined;
            form?: "call" | "decorate" | undefined;
            key_labels?: Record<string, "name" | "not_name"> | undefined;
            name_scope?: {
                scope: "global" | "service";
                namespace: string | null;
            } | undefined;
            of?: string | undefined;
            on?: "both" | "export" | "instance" | undefined;
            mounts?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            merges?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            picker?: string | undefined;
            collects?: {
                join: string;
                arg: number;
                key?: string | undefined;
            } | undefined;
            passes?: boolean | undefined;
        } | {
            kind: "op";
            member: string | null;
            op: "request" | "send" | "receive" | "execute";
            name?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                document?: "graphql" | undefined;
                ref?: "function" | "class" | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | {
                parts: {
                    arg: number;
                    at?: string[] | undefined;
                    key?: string | undefined;
                    each?: boolean | undefined;
                    handle?: boolean | undefined;
                    or_file?: boolean | undefined;
                }[];
                join?: string | undefined;
            } | {
                literal: string;
            } | {
                bound: "decorated" | "maker" | "scope" | "key" | "path" | "export" | "subscribed";
                join?: string | undefined;
                at?: string[] | undefined;
                param?: number | undefined;
            } | {
                first: ({
                    arg: number;
                    key?: string | undefined;
                } | {
                    bound: "decorated";
                })[];
            } | undefined;
            request?: {
                param: number;
                body?: string[] | undefined;
                url?: string[] | undefined;
                path?: string[] | undefined;
                method?: string[] | undefined;
            } | undefined;
            path?: string[] | undefined;
            result?: {
                whole: true;
            } | {
                member: [string];
            } | {
                parse: string;
            } | undefined;
            method_default?: string | undefined;
            base?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            runs?: {
                arg: number;
                key?: string | undefined;
                bound?: "maker" | undefined;
            } | undefined;
            yields?: {
                side: string;
                export: string;
                param: number;
                package?: string | undefined;
            } | undefined;
            form?: "call" | "new" | "set" | "override" | "decorate" | undefined;
            handler?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            key_labels?: Record<string, "name" | "not_name"> | undefined;
            name_scope?: {
                scope: "global" | "service";
                namespace: string | null;
            } | undefined;
            method?: string | undefined;
            payload?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            options?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            method_key?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            returns?: {
                result?: {
                    whole: true;
                } | {
                    member: [string];
                } | {
                    parse: string;
                } | undefined;
            } | undefined;
            when?: {
                literal: string;
                arg: number;
                key?: string | undefined;
            } | undefined;
            of?: string | undefined;
            on?: "both" | "export" | "instance" | undefined;
            ack?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            protocol?: "http" | "graphql" | "socket" | "rpc" | "message" | undefined;
            picker?: string | undefined;
        } | {
            kind: "result";
            member: string | null;
            result: {
                whole: true;
            } | {
                member: [string];
            } | {
                parse: string;
            };
            form: "make" | "op";
            path?: string[] | undefined;
            of?: string | undefined;
            on?: "both" | "export" | "instance" | undefined;
            picker?: string | undefined;
        } | {
            kind: "reserved";
            name: string;
            member: string;
            path?: string[] | undefined;
            of?: string | undefined;
            on?: "both" | "export" | "instance" | undefined;
            picker?: string | undefined;
        };
        role: "none" | "socket" | "http_client" | "graphql_client" | "broker" | "in_process_bus" | "server_framework" | "invoke_client";
    }[];
    scan_root?: string | undefined;
    budget_ms?: number | null | undefined;
}, {
    request_id: string;
    action: "verify_library_claims";
    from_dir: string;
    checks: {
        receiver: string;
        export: string;
        package: string;
        claim_id: string;
        claim: {
            kind: "make";
            member: string | null;
            form: "call" | "new";
            name?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                document?: "graphql" | undefined;
                ref?: "function" | "class" | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            result?: {
                whole: true;
            } | {
                member: [string];
            } | {
                parse: string;
            } | undefined;
            base?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            runs?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            prefix?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            handler?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            key_labels?: Record<string, "name" | "not_name"> | undefined;
            name_scope?: {
                scope: "global" | "service";
                namespace: string | null;
            } | undefined;
            picker?: string | undefined;
        } | {
            kind: "scope";
            member: string;
            name?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                document?: "graphql" | undefined;
                ref?: "function" | "class" | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            path?: string[] | undefined;
            runs?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            yields?: {
                side: string;
                export: string;
                param: number;
                package?: string | undefined;
            } | undefined;
            form?: "call" | "decorate" | undefined;
            key_labels?: Record<string, "name" | "not_name"> | undefined;
            name_scope?: {
                scope: "global" | "service";
                namespace: string | null;
            } | undefined;
            of?: string | undefined;
            on?: "both" | "export" | "instance" | undefined;
            mounts?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            merges?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            picker?: string | undefined;
            collects?: {
                join: string;
                arg: number;
                key?: string | undefined;
            } | undefined;
            passes?: boolean | undefined;
        } | {
            kind: "op";
            member: string | null;
            op: "request" | "send" | "receive" | "execute";
            name?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                document?: "graphql" | undefined;
                ref?: "function" | "class" | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | {
                parts: {
                    arg: number;
                    at?: string[] | undefined;
                    key?: string | undefined;
                    each?: boolean | undefined;
                    handle?: boolean | undefined;
                    or_file?: boolean | undefined;
                }[];
                join?: string | undefined;
            } | {
                literal: string;
            } | {
                bound: "decorated" | "maker" | "scope" | "key" | "path" | "export" | "subscribed";
                join?: string | undefined;
                at?: string[] | undefined;
                param?: number | undefined;
            } | {
                first: ({
                    arg: number;
                    key?: string | undefined;
                } | {
                    bound: "decorated";
                })[];
            } | undefined;
            request?: {
                param: number;
                body?: string[] | undefined;
                url?: string[] | undefined;
                path?: string[] | undefined;
                method?: string[] | undefined;
            } | undefined;
            path?: string[] | undefined;
            result?: {
                whole: true;
            } | {
                member: [string];
            } | {
                parse: string;
            } | undefined;
            method_default?: string | undefined;
            base?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            runs?: {
                arg: number;
                key?: string | undefined;
                bound?: "maker" | undefined;
            } | undefined;
            yields?: {
                side: string;
                export: string;
                param: number;
                package?: string | undefined;
            } | undefined;
            form?: "call" | "new" | "set" | "override" | "decorate" | undefined;
            handler?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            key_labels?: Record<string, "name" | "not_name"> | undefined;
            name_scope?: {
                scope: "global" | "service";
                namespace: string | null;
            } | undefined;
            method?: string | undefined;
            payload?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            options?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            method_key?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            returns?: {
                result?: {
                    whole: true;
                } | {
                    member: [string];
                } | {
                    parse: string;
                } | undefined;
            } | undefined;
            when?: {
                literal: string;
                arg: number;
                key?: string | undefined;
            } | undefined;
            of?: string | undefined;
            on?: "both" | "export" | "instance" | undefined;
            ack?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            protocol?: "http" | "graphql" | "socket" | "rpc" | "message" | undefined;
            picker?: string | undefined;
        } | {
            kind: "result";
            member: string | null;
            result: {
                whole: true;
            } | {
                member: [string];
            } | {
                parse: string;
            };
            form: "make" | "op";
            path?: string[] | undefined;
            of?: string | undefined;
            on?: "both" | "export" | "instance" | undefined;
            picker?: string | undefined;
        } | {
            kind: "reserved";
            name: string;
            member: string;
            path?: string[] | undefined;
            of?: string | undefined;
            on?: "both" | "export" | "instance" | undefined;
            picker?: string | undefined;
        };
        role: "none" | "socket" | "http_client" | "graphql_client" | "broker" | "in_process_bus" | "server_framework" | "invoke_client";
    }[];
    scan_root?: string | undefined;
    budget_ms?: number | null | undefined;
}>;
export declare const ListLibrarySurfaceRequestSchema: z.ZodObject<{
    request_id: z.ZodString;
} & {
    action: z.ZodLiteral<"list_library_surface">;
    from_dir: z.ZodString;
    packages: z.ZodArray<z.ZodString, "many">;
    max_entries: z.ZodOptional<z.ZodNumber>;
    exports: z.ZodOptional<z.ZodRecord<z.ZodString, z.ZodArray<z.ZodString, "many">>>;
    handlers: z.ZodOptional<z.ZodBoolean>;
    types: z.ZodOptional<z.ZodBoolean>;
    result_candidates: z.ZodOptional<z.ZodBoolean>;
    max_handler_members: z.ZodOptional<z.ZodNumber>;
    foreign_maker_budget: z.ZodOptional<z.ZodNumber>;
}, "strip", z.ZodTypeAny, {
    packages: string[];
    request_id: string;
    action: "list_library_surface";
    from_dir: string;
    types?: boolean | undefined;
    handlers?: boolean | undefined;
    result_candidates?: boolean | undefined;
    exports?: Record<string, string[]> | undefined;
    max_entries?: number | undefined;
    max_handler_members?: number | undefined;
    foreign_maker_budget?: number | undefined;
}, {
    packages: string[];
    request_id: string;
    action: "list_library_surface";
    from_dir: string;
    types?: boolean | undefined;
    handlers?: boolean | undefined;
    result_candidates?: boolean | undefined;
    exports?: Record<string, string[]> | undefined;
    max_entries?: number | undefined;
    max_handler_members?: number | undefined;
    foreign_maker_budget?: number | undefined;
}>;
/**
 * The main sidecar request schema - a discriminated union on the 'action' field
 */
export declare const SidecarRequestSchema: z.ZodDiscriminatedUnion<"action", [z.ZodObject<{
    request_id: z.ZodString;
} & {
    action: z.ZodLiteral<"init">;
    repo_root: z.ZodString;
    tsconfig_path: z.ZodOptional<z.ZodString>;
    scan_root: z.ZodOptional<z.ZodString>;
    tsconfig_snapshot: z.ZodOptional<z.ZodObject<{
        compilerOptions: z.ZodObject<{
            module: z.ZodOptional<z.ZodString>;
            moduleResolution: z.ZodOptional<z.ZodString>;
            target: z.ZodOptional<z.ZodString>;
            lib: z.ZodOptional<z.ZodArray<z.ZodString, "many">>;
            types: z.ZodOptional<z.ZodArray<z.ZodString, "many">>;
            typeRoots: z.ZodOptional<z.ZodArray<z.ZodString, "many">>;
            jsx: z.ZodOptional<z.ZodString>;
            strict: z.ZodOptional<z.ZodBoolean>;
            esModuleInterop: z.ZodOptional<z.ZodBoolean>;
            skipLibCheck: z.ZodOptional<z.ZodBoolean>;
            declaration: z.ZodOptional<z.ZodBoolean>;
            declarationMap: z.ZodOptional<z.ZodBoolean>;
            paths: z.ZodOptional<z.ZodRecord<z.ZodString, z.ZodArray<z.ZodString, "many">>>;
            baseUrl: z.ZodOptional<z.ZodString>;
        }, "passthrough", z.ZodTypeAny, z.objectOutputType<{
            module: z.ZodOptional<z.ZodString>;
            moduleResolution: z.ZodOptional<z.ZodString>;
            target: z.ZodOptional<z.ZodString>;
            lib: z.ZodOptional<z.ZodArray<z.ZodString, "many">>;
            types: z.ZodOptional<z.ZodArray<z.ZodString, "many">>;
            typeRoots: z.ZodOptional<z.ZodArray<z.ZodString, "many">>;
            jsx: z.ZodOptional<z.ZodString>;
            strict: z.ZodOptional<z.ZodBoolean>;
            esModuleInterop: z.ZodOptional<z.ZodBoolean>;
            skipLibCheck: z.ZodOptional<z.ZodBoolean>;
            declaration: z.ZodOptional<z.ZodBoolean>;
            declarationMap: z.ZodOptional<z.ZodBoolean>;
            paths: z.ZodOptional<z.ZodRecord<z.ZodString, z.ZodArray<z.ZodString, "many">>>;
            baseUrl: z.ZodOptional<z.ZodString>;
        }, z.ZodTypeAny, "passthrough">, z.objectInputType<{
            module: z.ZodOptional<z.ZodString>;
            moduleResolution: z.ZodOptional<z.ZodString>;
            target: z.ZodOptional<z.ZodString>;
            lib: z.ZodOptional<z.ZodArray<z.ZodString, "many">>;
            types: z.ZodOptional<z.ZodArray<z.ZodString, "many">>;
            typeRoots: z.ZodOptional<z.ZodArray<z.ZodString, "many">>;
            jsx: z.ZodOptional<z.ZodString>;
            strict: z.ZodOptional<z.ZodBoolean>;
            esModuleInterop: z.ZodOptional<z.ZodBoolean>;
            skipLibCheck: z.ZodOptional<z.ZodBoolean>;
            declaration: z.ZodOptional<z.ZodBoolean>;
            declarationMap: z.ZodOptional<z.ZodBoolean>;
            paths: z.ZodOptional<z.ZodRecord<z.ZodString, z.ZodArray<z.ZodString, "many">>>;
            baseUrl: z.ZodOptional<z.ZodString>;
        }, z.ZodTypeAny, "passthrough">>;
    }, "strip", z.ZodTypeAny, {
        compilerOptions: {
            module?: string | undefined;
            moduleResolution?: string | undefined;
            target?: string | undefined;
            lib?: string[] | undefined;
            types?: string[] | undefined;
            typeRoots?: string[] | undefined;
            jsx?: string | undefined;
            strict?: boolean | undefined;
            esModuleInterop?: boolean | undefined;
            skipLibCheck?: boolean | undefined;
            declaration?: boolean | undefined;
            declarationMap?: boolean | undefined;
            paths?: Record<string, string[]> | undefined;
            baseUrl?: string | undefined;
        } & {
            [k: string]: unknown;
        };
    }, {
        compilerOptions: {
            module?: string | undefined;
            moduleResolution?: string | undefined;
            target?: string | undefined;
            lib?: string[] | undefined;
            types?: string[] | undefined;
            typeRoots?: string[] | undefined;
            jsx?: string | undefined;
            strict?: boolean | undefined;
            esModuleInterop?: boolean | undefined;
            skipLibCheck?: boolean | undefined;
            declaration?: boolean | undefined;
            declarationMap?: boolean | undefined;
            paths?: Record<string, string[]> | undefined;
            baseUrl?: string | undefined;
        } & {
            [k: string]: unknown;
        };
    }>>;
    pinned_dependencies: z.ZodOptional<z.ZodRecord<z.ZodString, z.ZodString>>;
    workspace_source: z.ZodOptional<z.ZodBoolean>;
}, "strip", z.ZodTypeAny, {
    request_id: string;
    action: "init";
    repo_root: string;
    tsconfig_path?: string | undefined;
    scan_root?: string | undefined;
    tsconfig_snapshot?: {
        compilerOptions: {
            module?: string | undefined;
            moduleResolution?: string | undefined;
            target?: string | undefined;
            lib?: string[] | undefined;
            types?: string[] | undefined;
            typeRoots?: string[] | undefined;
            jsx?: string | undefined;
            strict?: boolean | undefined;
            esModuleInterop?: boolean | undefined;
            skipLibCheck?: boolean | undefined;
            declaration?: boolean | undefined;
            declarationMap?: boolean | undefined;
            paths?: Record<string, string[]> | undefined;
            baseUrl?: string | undefined;
        } & {
            [k: string]: unknown;
        };
    } | undefined;
    pinned_dependencies?: Record<string, string> | undefined;
    workspace_source?: boolean | undefined;
}, {
    request_id: string;
    action: "init";
    repo_root: string;
    tsconfig_path?: string | undefined;
    scan_root?: string | undefined;
    tsconfig_snapshot?: {
        compilerOptions: {
            module?: string | undefined;
            moduleResolution?: string | undefined;
            target?: string | undefined;
            lib?: string[] | undefined;
            types?: string[] | undefined;
            typeRoots?: string[] | undefined;
            jsx?: string | undefined;
            strict?: boolean | undefined;
            esModuleInterop?: boolean | undefined;
            skipLibCheck?: boolean | undefined;
            declaration?: boolean | undefined;
            declarationMap?: boolean | undefined;
            paths?: Record<string, string[]> | undefined;
            baseUrl?: string | undefined;
        } & {
            [k: string]: unknown;
        };
    } | undefined;
    pinned_dependencies?: Record<string, string> | undefined;
    workspace_source?: boolean | undefined;
}>, z.ZodObject<{
    request_id: z.ZodString;
} & {
    action: z.ZodLiteral<"bundle">;
    symbols: z.ZodArray<z.ZodObject<{
        symbol_name: z.ZodString;
        source_file: z.ZodString;
        alias: z.ZodOptional<z.ZodString>;
        array_depth: z.ZodOptional<z.ZodNumber>;
        consumer_response: z.ZodOptional<z.ZodBoolean>;
    }, "strip", z.ZodTypeAny, {
        symbol_name: string;
        source_file: string;
        consumer_response?: boolean | undefined;
        alias?: string | undefined;
        array_depth?: number | undefined;
    }, {
        symbol_name: string;
        source_file: string;
        consumer_response?: boolean | undefined;
        alias?: string | undefined;
        array_depth?: number | undefined;
    }>, "many">;
}, "strip", z.ZodTypeAny, {
    request_id: string;
    action: "bundle";
    symbols: {
        symbol_name: string;
        source_file: string;
        consumer_response?: boolean | undefined;
        alias?: string | undefined;
        array_depth?: number | undefined;
    }[];
}, {
    request_id: string;
    action: "bundle";
    symbols: {
        symbol_name: string;
        source_file: string;
        consumer_response?: boolean | undefined;
        alias?: string | undefined;
        array_depth?: number | undefined;
    }[];
}>, z.ZodObject<{
    request_id: z.ZodString;
} & {
    action: z.ZodLiteral<"capture_v2">;
    repo_root: z.ZodString;
    service_name: z.ZodString;
    anchors: z.ZodArray<z.ZodDiscriminatedUnion<"kind", [z.ZodObject<{
        kind: z.ZodLiteral<"symbol">;
        alias: z.ZodString;
        symbol_name: z.ZodString;
        source_file: z.ZodString;
        anchor_origin: z.ZodEnum<["llm-symbol", "deterministic-infer", "anchor-backfill", "manifest-placeholder"]>;
        array_depth: z.ZodOptional<z.ZodNumber>;
        consumer_response: z.ZodOptional<z.ZodBoolean>;
        absent_body_tolerated: z.ZodOptional<z.ZodLiteral<true>>;
    }, "strip", z.ZodTypeAny, {
        kind: "symbol";
        alias: string;
        symbol_name: string;
        source_file: string;
        anchor_origin: "llm-symbol" | "deterministic-infer" | "anchor-backfill" | "manifest-placeholder";
        consumer_response?: boolean | undefined;
        absent_body_tolerated?: true | undefined;
        array_depth?: number | undefined;
    }, {
        kind: "symbol";
        alias: string;
        symbol_name: string;
        source_file: string;
        anchor_origin: "llm-symbol" | "deterministic-infer" | "anchor-backfill" | "manifest-placeholder";
        consumer_response?: boolean | undefined;
        absent_body_tolerated?: true | undefined;
        array_depth?: number | undefined;
    }>, z.ZodObject<{
        kind: z.ZodLiteral<"handler_return">;
        alias: z.ZodString;
        symbol_name: z.ZodString;
        source_file: z.ZodString;
        anchor_origin: z.ZodEnum<["llm-symbol", "deterministic-infer", "anchor-backfill", "manifest-placeholder"]>;
    }, "strip", z.ZodTypeAny, {
        kind: "handler_return";
        alias: string;
        symbol_name: string;
        source_file: string;
        anchor_origin: "llm-symbol" | "deterministic-infer" | "anchor-backfill" | "manifest-placeholder";
    }, {
        kind: "handler_return";
        alias: string;
        symbol_name: string;
        source_file: string;
        anchor_origin: "llm-symbol" | "deterministic-infer" | "anchor-backfill" | "manifest-placeholder";
    }>, z.ZodObject<{
        kind: z.ZodLiteral<"infer">;
        alias: z.ZodString;
        source_file: z.ZodString;
        anchor_origin: z.ZodEnum<["llm-symbol", "deterministic-infer", "anchor-backfill", "manifest-placeholder"]>;
        span_start: z.ZodOptional<z.ZodNumber>;
        span_end: z.ZodOptional<z.ZodNumber>;
        line_number: z.ZodOptional<z.ZodNumber>;
        expression_text: z.ZodOptional<z.ZodString>;
        param_name: z.ZodOptional<z.ZodString>;
        unwrap: z.ZodOptional<z.ZodEnum<["awaited", "none"]>>;
        result: z.ZodOptional<z.ZodUnion<[z.ZodObject<{
            whole: z.ZodLiteral<true>;
        }, "strict", z.ZodTypeAny, {
            whole: true;
        }, {
            whole: true;
        }>, z.ZodObject<{
            member: z.ZodTuple<[z.ZodString], null>;
        }, "strict", z.ZodTypeAny, {
            member: [string];
        }, {
            member: [string];
        }>, z.ZodObject<{
            parse: z.ZodString;
        }, "strict", z.ZodTypeAny, {
            parse: string;
        }, {
            parse: string;
        }>]>>;
    }, "strip", z.ZodTypeAny, {
        kind: "infer";
        alias: string;
        source_file: string;
        anchor_origin: "llm-symbol" | "deterministic-infer" | "anchor-backfill" | "manifest-placeholder";
        result?: {
            whole: true;
        } | {
            member: [string];
        } | {
            parse: string;
        } | undefined;
        param_name?: string | undefined;
        line_number?: number | undefined;
        span_start?: number | undefined;
        span_end?: number | undefined;
        expression_text?: string | undefined;
        unwrap?: "awaited" | "none" | undefined;
    }, {
        kind: "infer";
        alias: string;
        source_file: string;
        anchor_origin: "llm-symbol" | "deterministic-infer" | "anchor-backfill" | "manifest-placeholder";
        result?: {
            whole: true;
        } | {
            member: [string];
        } | {
            parse: string;
        } | undefined;
        param_name?: string | undefined;
        line_number?: number | undefined;
        span_start?: number | undefined;
        span_end?: number | undefined;
        expression_text?: string | undefined;
        unwrap?: "awaited" | "none" | undefined;
    }>, z.ZodObject<{
        kind: z.ZodLiteral<"literal">;
        alias: z.ZodString;
        type_text: z.ZodString;
        anchor_origin: z.ZodEnum<["llm-symbol", "deterministic-infer", "anchor-backfill", "manifest-placeholder"]>;
        source_file: z.ZodOptional<z.ZodString>;
        printed_names: z.ZodOptional<z.ZodArray<z.ZodObject<{
            name: z.ZodString;
            file: z.ZodString;
            export_path: z.ZodArray<z.ZodString, "many">;
        }, "strip", z.ZodTypeAny, {
            name: string;
            file: string;
            export_path: string[];
        }, {
            name: string;
            file: string;
            export_path: string[];
        }>, "many">>;
        raw_text_read: z.ZodOptional<z.ZodLiteral<true>>;
        absent_body_tolerated: z.ZodOptional<z.ZodLiteral<true>>;
    }, "strip", z.ZodTypeAny, {
        kind: "literal";
        alias: string;
        anchor_origin: "llm-symbol" | "deterministic-infer" | "anchor-backfill" | "manifest-placeholder";
        type_text: string;
        raw_text_read?: true | undefined;
        absent_body_tolerated?: true | undefined;
        source_file?: string | undefined;
        printed_names?: {
            name: string;
            file: string;
            export_path: string[];
        }[] | undefined;
    }, {
        kind: "literal";
        alias: string;
        anchor_origin: "llm-symbol" | "deterministic-infer" | "anchor-backfill" | "manifest-placeholder";
        type_text: string;
        raw_text_read?: true | undefined;
        absent_body_tolerated?: true | undefined;
        source_file?: string | undefined;
        printed_names?: {
            name: string;
            file: string;
            export_path: string[];
        }[] | undefined;
    }>]>, "many">;
    out_dir: z.ZodString;
    tsconfig_path: z.ZodOptional<z.ZodString>;
    scan_root: z.ZodOptional<z.ZodString>;
    backfill_texts: z.ZodOptional<z.ZodRecord<z.ZodString, z.ZodString>>;
    backfill_out_dir: z.ZodOptional<z.ZodString>;
}, "strip", z.ZodTypeAny, {
    anchors: ({
        kind: "symbol";
        alias: string;
        symbol_name: string;
        source_file: string;
        anchor_origin: "llm-symbol" | "deterministic-infer" | "anchor-backfill" | "manifest-placeholder";
        consumer_response?: boolean | undefined;
        absent_body_tolerated?: true | undefined;
        array_depth?: number | undefined;
    } | {
        kind: "handler_return";
        alias: string;
        symbol_name: string;
        source_file: string;
        anchor_origin: "llm-symbol" | "deterministic-infer" | "anchor-backfill" | "manifest-placeholder";
    } | {
        kind: "infer";
        alias: string;
        source_file: string;
        anchor_origin: "llm-symbol" | "deterministic-infer" | "anchor-backfill" | "manifest-placeholder";
        result?: {
            whole: true;
        } | {
            member: [string];
        } | {
            parse: string;
        } | undefined;
        param_name?: string | undefined;
        line_number?: number | undefined;
        span_start?: number | undefined;
        span_end?: number | undefined;
        expression_text?: string | undefined;
        unwrap?: "awaited" | "none" | undefined;
    } | {
        kind: "literal";
        alias: string;
        anchor_origin: "llm-symbol" | "deterministic-infer" | "anchor-backfill" | "manifest-placeholder";
        type_text: string;
        raw_text_read?: true | undefined;
        absent_body_tolerated?: true | undefined;
        source_file?: string | undefined;
        printed_names?: {
            name: string;
            file: string;
            export_path: string[];
        }[] | undefined;
    })[];
    request_id: string;
    action: "capture_v2";
    repo_root: string;
    service_name: string;
    out_dir: string;
    tsconfig_path?: string | undefined;
    scan_root?: string | undefined;
    backfill_texts?: Record<string, string> | undefined;
    backfill_out_dir?: string | undefined;
}, {
    anchors: ({
        kind: "symbol";
        alias: string;
        symbol_name: string;
        source_file: string;
        anchor_origin: "llm-symbol" | "deterministic-infer" | "anchor-backfill" | "manifest-placeholder";
        consumer_response?: boolean | undefined;
        absent_body_tolerated?: true | undefined;
        array_depth?: number | undefined;
    } | {
        kind: "handler_return";
        alias: string;
        symbol_name: string;
        source_file: string;
        anchor_origin: "llm-symbol" | "deterministic-infer" | "anchor-backfill" | "manifest-placeholder";
    } | {
        kind: "infer";
        alias: string;
        source_file: string;
        anchor_origin: "llm-symbol" | "deterministic-infer" | "anchor-backfill" | "manifest-placeholder";
        result?: {
            whole: true;
        } | {
            member: [string];
        } | {
            parse: string;
        } | undefined;
        param_name?: string | undefined;
        line_number?: number | undefined;
        span_start?: number | undefined;
        span_end?: number | undefined;
        expression_text?: string | undefined;
        unwrap?: "awaited" | "none" | undefined;
    } | {
        kind: "literal";
        alias: string;
        anchor_origin: "llm-symbol" | "deterministic-infer" | "anchor-backfill" | "manifest-placeholder";
        type_text: string;
        raw_text_read?: true | undefined;
        absent_body_tolerated?: true | undefined;
        source_file?: string | undefined;
        printed_names?: {
            name: string;
            file: string;
            export_path: string[];
        }[] | undefined;
    })[];
    request_id: string;
    action: "capture_v2";
    repo_root: string;
    service_name: string;
    out_dir: string;
    tsconfig_path?: string | undefined;
    scan_root?: string | undefined;
    backfill_texts?: Record<string, string> | undefined;
    backfill_out_dir?: string | undefined;
}>, z.ZodObject<{
    request_id: z.ZodString;
} & {
    action: z.ZodLiteral<"check_v2">;
    stubs: z.ZodArray<z.ZodObject<{
        service_name: z.ZodString;
        stub_dir: z.ZodString;
    }, "strip", z.ZodTypeAny, {
        service_name: string;
        stub_dir: string;
    }, {
        service_name: string;
        stub_dir: string;
    }>, "many">;
    pairs: z.ZodArray<z.ZodEffects<z.ZodObject<{
        pair_key: z.ZodString;
        protocol: z.ZodEnum<["http", "graphql", "socket", "pubsub"]>;
        type_kind: z.ZodEnum<["request", "response", "both"]>;
        producer: z.ZodObject<{
            service_name: z.ZodString;
            alias: z.ZodString;
        }, "strip", z.ZodTypeAny, {
            alias: string;
            service_name: string;
        }, {
            alias: string;
            service_name: string;
        }>;
        consumer: z.ZodObject<{
            service_name: z.ZodString;
            alias: z.ZodString;
        }, "strip", z.ZodTypeAny, {
            alias: string;
            service_name: string;
        }, {
            alias: string;
            service_name: string;
        }>;
        topic: z.ZodOptional<z.ZodString>;
    }, "strip", z.ZodTypeAny, {
        producer: {
            alias: string;
            service_name: string;
        };
        consumer: {
            alias: string;
            service_name: string;
        };
        protocol: "http" | "graphql" | "socket" | "pubsub";
        pair_key: string;
        type_kind: "request" | "response" | "both";
        topic?: string | undefined;
    }, {
        producer: {
            alias: string;
            service_name: string;
        };
        consumer: {
            alias: string;
            service_name: string;
        };
        protocol: "http" | "graphql" | "socket" | "pubsub";
        pair_key: string;
        type_kind: "request" | "response" | "both";
        topic?: string | undefined;
    }>, {
        producer: {
            alias: string;
            service_name: string;
        };
        consumer: {
            alias: string;
            service_name: string;
        };
        protocol: "http" | "graphql" | "socket" | "pubsub";
        pair_key: string;
        type_kind: "request" | "response" | "both";
        topic?: string | undefined;
    }, {
        producer: {
            alias: string;
            service_name: string;
        };
        consumer: {
            alias: string;
            service_name: string;
        };
        protocol: "http" | "graphql" | "socket" | "pubsub";
        pair_key: string;
        type_kind: "request" | "response" | "both";
        topic?: string | undefined;
    }>, "many">;
    workspace_root: z.ZodOptional<z.ZodString>;
    keep_workspace: z.ZodOptional<z.ZodBoolean>;
}, "strip", z.ZodTypeAny, {
    pairs: {
        producer: {
            alias: string;
            service_name: string;
        };
        consumer: {
            alias: string;
            service_name: string;
        };
        protocol: "http" | "graphql" | "socket" | "pubsub";
        pair_key: string;
        type_kind: "request" | "response" | "both";
        topic?: string | undefined;
    }[];
    request_id: string;
    action: "check_v2";
    stubs: {
        service_name: string;
        stub_dir: string;
    }[];
    workspace_root?: string | undefined;
    keep_workspace?: boolean | undefined;
}, {
    pairs: {
        producer: {
            alias: string;
            service_name: string;
        };
        consumer: {
            alias: string;
            service_name: string;
        };
        protocol: "http" | "graphql" | "socket" | "pubsub";
        pair_key: string;
        type_kind: "request" | "response" | "both";
        topic?: string | undefined;
    }[];
    request_id: string;
    action: "check_v2";
    stubs: {
        service_name: string;
        stub_dir: string;
    }[];
    workspace_root?: string | undefined;
    keep_workspace?: boolean | undefined;
}>, z.ZodObject<{
    request_id: z.ZodString;
} & {
    action: z.ZodLiteral<"infer">;
    requests: z.ZodArray<z.ZodObject<{
        file_path: z.ZodString;
        line_number: z.ZodNumber;
        span_start: z.ZodOptional<z.ZodNumber>;
        span_end: z.ZodOptional<z.ZodNumber>;
        expression_text: z.ZodOptional<z.ZodString>;
        expression_line: z.ZodOptional<z.ZodNumber>;
        infer_kind: z.ZodEnum<["function_return", "expression", "call_result", "variable", "response_body", "request_body", "signature_return", "function_param", "receiver_type", "request_config"]>;
        alias: z.ZodOptional<z.ZodString>;
        param_name: z.ZodOptional<z.ZodString>;
        member: z.ZodOptional<z.ZodString>;
        result: z.ZodOptional<z.ZodUnion<[z.ZodObject<{
            whole: z.ZodLiteral<true>;
        }, "strict", z.ZodTypeAny, {
            whole: true;
        }, {
            whole: true;
        }>, z.ZodObject<{
            member: z.ZodTuple<[z.ZodString], null>;
        }, "strict", z.ZodTypeAny, {
            member: [string];
        }, {
            member: [string];
        }>, z.ZodObject<{
            parse: z.ZodString;
        }, "strict", z.ZodTypeAny, {
            parse: string;
        }, {
            parse: string;
        }>]>>;
        route_path: z.ZodOptional<z.ZodString>;
    }, "strip", z.ZodTypeAny, {
        file_path: string;
        line_number: number;
        infer_kind: "function_return" | "expression" | "call_result" | "variable" | "response_body" | "request_body" | "signature_return" | "function_param" | "receiver_type" | "request_config";
        member?: string | undefined;
        result?: {
            whole: true;
        } | {
            member: [string];
        } | {
            parse: string;
        } | undefined;
        param_name?: string | undefined;
        alias?: string | undefined;
        span_start?: number | undefined;
        span_end?: number | undefined;
        expression_text?: string | undefined;
        expression_line?: number | undefined;
        route_path?: string | undefined;
    }, {
        file_path: string;
        line_number: number;
        infer_kind: "function_return" | "expression" | "call_result" | "variable" | "response_body" | "request_body" | "signature_return" | "function_param" | "receiver_type" | "request_config";
        member?: string | undefined;
        result?: {
            whole: true;
        } | {
            member: [string];
        } | {
            parse: string;
        } | undefined;
        param_name?: string | undefined;
        alias?: string | undefined;
        span_start?: number | undefined;
        span_end?: number | undefined;
        expression_text?: string | undefined;
        expression_line?: number | undefined;
        route_path?: string | undefined;
    }>, "many">;
    extraction_config: z.ZodOptional<z.ZodObject<{
        rules: z.ZodArray<z.ZodObject<{
            wrapperSymbols: z.ZodOptional<z.ZodArray<z.ZodString, "many">>;
            machineryIndicators: z.ZodOptional<z.ZodArray<z.ZodString, "many">>;
            originModuleGlobs: z.ZodOptional<z.ZodArray<z.ZodString, "many">>;
            payloadGenericIndex: z.ZodOptional<z.ZodNumber>;
            payloadPropertyPath: z.ZodOptional<z.ZodArray<z.ZodString, "many">>;
            unwrapRecursively: z.ZodOptional<z.ZodBoolean>;
            maxDepth: z.ZodOptional<z.ZodNumber>;
        }, "strip", z.ZodTypeAny, {
            wrapperSymbols?: string[] | undefined;
            machineryIndicators?: string[] | undefined;
            originModuleGlobs?: string[] | undefined;
            payloadGenericIndex?: number | undefined;
            payloadPropertyPath?: string[] | undefined;
            unwrapRecursively?: boolean | undefined;
            maxDepth?: number | undefined;
        }, {
            wrapperSymbols?: string[] | undefined;
            machineryIndicators?: string[] | undefined;
            originModuleGlobs?: string[] | undefined;
            payloadGenericIndex?: number | undefined;
            payloadPropertyPath?: string[] | undefined;
            unwrapRecursively?: boolean | undefined;
            maxDepth?: number | undefined;
        }>, "many">;
    }, "strip", z.ZodTypeAny, {
        rules: {
            wrapperSymbols?: string[] | undefined;
            machineryIndicators?: string[] | undefined;
            originModuleGlobs?: string[] | undefined;
            payloadGenericIndex?: number | undefined;
            payloadPropertyPath?: string[] | undefined;
            unwrapRecursively?: boolean | undefined;
            maxDepth?: number | undefined;
        }[];
    }, {
        rules: {
            wrapperSymbols?: string[] | undefined;
            machineryIndicators?: string[] | undefined;
            originModuleGlobs?: string[] | undefined;
            payloadGenericIndex?: number | undefined;
            payloadPropertyPath?: string[] | undefined;
            unwrapRecursively?: boolean | undefined;
            maxDepth?: number | undefined;
        }[];
    }>>;
    model_symbols: z.ZodOptional<z.ZodArray<z.ZodObject<{
        alias: z.ZodString;
        symbol_name: z.ZodString;
        source_file: z.ZodString;
    }, "strip", z.ZodTypeAny, {
        alias: string;
        symbol_name: string;
        source_file: string;
    }, {
        alias: string;
        symbol_name: string;
        source_file: string;
    }>, "many">>;
    server_claims: z.ZodOptional<z.ZodLazy<z.ZodObject<{
        from_dir: z.ZodString;
        scan_root: z.ZodOptional<z.ZodString>;
        checks: z.ZodArray<z.ZodUnknown, "many">;
        type_packages: z.ZodOptional<z.ZodRecord<z.ZodString, z.ZodArray<z.ZodString, "many">>>;
    }, "strict", z.ZodTypeAny, {
        from_dir: string;
        checks: unknown[];
        scan_root?: string | undefined;
        type_packages?: Record<string, string[]> | undefined;
    }, {
        from_dir: string;
        checks: unknown[];
        scan_root?: string | undefined;
        type_packages?: Record<string, string[]> | undefined;
    }>>>;
    framework_packages: z.ZodOptional<z.ZodArray<z.ZodString, "many">>;
}, "strip", z.ZodTypeAny, {
    request_id: string;
    action: "infer";
    requests: {
        file_path: string;
        line_number: number;
        infer_kind: "function_return" | "expression" | "call_result" | "variable" | "response_body" | "request_body" | "signature_return" | "function_param" | "receiver_type" | "request_config";
        member?: string | undefined;
        result?: {
            whole: true;
        } | {
            member: [string];
        } | {
            parse: string;
        } | undefined;
        param_name?: string | undefined;
        alias?: string | undefined;
        span_start?: number | undefined;
        span_end?: number | undefined;
        expression_text?: string | undefined;
        expression_line?: number | undefined;
        route_path?: string | undefined;
    }[];
    extraction_config?: {
        rules: {
            wrapperSymbols?: string[] | undefined;
            machineryIndicators?: string[] | undefined;
            originModuleGlobs?: string[] | undefined;
            payloadGenericIndex?: number | undefined;
            payloadPropertyPath?: string[] | undefined;
            unwrapRecursively?: boolean | undefined;
            maxDepth?: number | undefined;
        }[];
    } | undefined;
    model_symbols?: {
        alias: string;
        symbol_name: string;
        source_file: string;
    }[] | undefined;
    server_claims?: {
        from_dir: string;
        checks: unknown[];
        scan_root?: string | undefined;
        type_packages?: Record<string, string[]> | undefined;
    } | undefined;
    framework_packages?: string[] | undefined;
}, {
    request_id: string;
    action: "infer";
    requests: {
        file_path: string;
        line_number: number;
        infer_kind: "function_return" | "expression" | "call_result" | "variable" | "response_body" | "request_body" | "signature_return" | "function_param" | "receiver_type" | "request_config";
        member?: string | undefined;
        result?: {
            whole: true;
        } | {
            member: [string];
        } | {
            parse: string;
        } | undefined;
        param_name?: string | undefined;
        alias?: string | undefined;
        span_start?: number | undefined;
        span_end?: number | undefined;
        expression_text?: string | undefined;
        expression_line?: number | undefined;
        route_path?: string | undefined;
    }[];
    extraction_config?: {
        rules: {
            wrapperSymbols?: string[] | undefined;
            machineryIndicators?: string[] | undefined;
            originModuleGlobs?: string[] | undefined;
            payloadGenericIndex?: number | undefined;
            payloadPropertyPath?: string[] | undefined;
            unwrapRecursively?: boolean | undefined;
            maxDepth?: number | undefined;
        }[];
    } | undefined;
    model_symbols?: {
        alias: string;
        symbol_name: string;
        source_file: string;
    }[] | undefined;
    server_claims?: {
        from_dir: string;
        checks: unknown[];
        scan_root?: string | undefined;
        type_packages?: Record<string, string[]> | undefined;
    } | undefined;
    framework_packages?: string[] | undefined;
}>, z.ZodObject<{
    request_id: z.ZodString;
} & {
    action: z.ZodLiteral<"resolve_definitions">;
    stub_dir: z.ZodString;
    aliases: z.ZodArray<z.ZodString, "many">;
}, "strip", z.ZodTypeAny, {
    request_id: string;
    action: "resolve_definitions";
    stub_dir: string;
    aliases: string[];
}, {
    request_id: string;
    action: "resolve_definitions";
    stub_dir: string;
    aliases: string[];
}>, z.ZodObject<{
    request_id: z.ZodString;
} & {
    action: z.ZodLiteral<"retype_check">;
    items: z.ZodArray<z.ZodObject<{
        item_id: z.ZodString;
        file_path: z.ZodString;
        line_number: z.ZodNumber;
        span_start: z.ZodOptional<z.ZodNumber>;
        span_end: z.ZodOptional<z.ZodNumber>;
        expression_text: z.ZodOptional<z.ZodString>;
        expression_line: z.ZodOptional<z.ZodNumber>;
    } & {
        producer_type: z.ZodString;
        producer_unwidened_type: z.ZodOptional<z.ZodString>;
        wire: z.ZodBoolean;
    }, "strip", z.ZodTypeAny, {
        wire: boolean;
        file_path: string;
        line_number: number;
        item_id: string;
        producer_type: string;
        span_start?: number | undefined;
        span_end?: number | undefined;
        expression_text?: string | undefined;
        expression_line?: number | undefined;
        producer_unwidened_type?: string | undefined;
    }, {
        wire: boolean;
        file_path: string;
        line_number: number;
        item_id: string;
        producer_type: string;
        span_start?: number | undefined;
        span_end?: number | undefined;
        expression_text?: string | undefined;
        expression_line?: number | undefined;
        producer_unwidened_type?: string | undefined;
    }>, "many">;
    budget_ms: z.ZodOptional<z.ZodNumber>;
}, "strip", z.ZodTypeAny, {
    request_id: string;
    action: "retype_check";
    items: {
        wire: boolean;
        file_path: string;
        line_number: number;
        item_id: string;
        producer_type: string;
        span_start?: number | undefined;
        span_end?: number | undefined;
        expression_text?: string | undefined;
        expression_line?: number | undefined;
        producer_unwidened_type?: string | undefined;
    }[];
    budget_ms?: number | undefined;
}, {
    request_id: string;
    action: "retype_check";
    items: {
        wire: boolean;
        file_path: string;
        line_number: number;
        item_id: string;
        producer_type: string;
        span_start?: number | undefined;
        span_end?: number | undefined;
        expression_text?: string | undefined;
        expression_line?: number | undefined;
        producer_unwidened_type?: string | undefined;
    }[];
    budget_ms?: number | undefined;
}>, z.ZodObject<{
    request_id: z.ZodString;
} & {
    action: z.ZodLiteral<"retype_prescreen">;
    items: z.ZodArray<z.ZodObject<{
        item_id: z.ZodString;
        file_path: z.ZodString;
        line_number: z.ZodNumber;
        span_start: z.ZodOptional<z.ZodNumber>;
        span_end: z.ZodOptional<z.ZodNumber>;
        expression_text: z.ZodOptional<z.ZodString>;
        expression_line: z.ZodOptional<z.ZodNumber>;
    }, "strip", z.ZodTypeAny, {
        file_path: string;
        line_number: number;
        item_id: string;
        span_start?: number | undefined;
        span_end?: number | undefined;
        expression_text?: string | undefined;
        expression_line?: number | undefined;
    }, {
        file_path: string;
        line_number: number;
        item_id: string;
        span_start?: number | undefined;
        span_end?: number | undefined;
        expression_text?: string | undefined;
        expression_line?: number | undefined;
    }>, "many">;
}, "strip", z.ZodTypeAny, {
    request_id: string;
    action: "retype_prescreen";
    items: {
        file_path: string;
        line_number: number;
        item_id: string;
        span_start?: number | undefined;
        span_end?: number | undefined;
        expression_text?: string | undefined;
        expression_line?: number | undefined;
    }[];
}, {
    request_id: string;
    action: "retype_prescreen";
    items: {
        file_path: string;
        line_number: number;
        item_id: string;
        span_start?: number | undefined;
        span_end?: number | undefined;
        expression_text?: string | undefined;
        expression_line?: number | undefined;
    }[];
}>, z.ZodObject<{
    request_id: z.ZodString;
} & {
    action: z.ZodLiteral<"verify_client_semantics">;
    from_dir: z.ZodString;
    checks: z.ZodArray<z.ZodObject<{
        claim_id: z.ZodString;
        package: z.ZodString;
        export: z.ZodString;
        receiver: z.ZodString;
        claim: z.ZodDiscriminatedUnion<"kind", [z.ZodObject<{
            kind: z.ZodLiteral<"factory">;
            member: z.ZodString;
            base_url_key: z.ZodString;
        }, "strip", z.ZodTypeAny, {
            kind: "factory";
            member: string;
            base_url_key: string;
        }, {
            kind: "factory";
            member: string;
            base_url_key: string;
        }>, z.ZodObject<{
            kind: z.ZodLiteral<"verb">;
            member: z.ZodString;
            method: z.ZodString;
        }, "strip", z.ZodTypeAny, {
            kind: "verb";
            member: string;
            method: string;
        }, {
            kind: "verb";
            member: string;
            method: string;
        }>, z.ZodObject<{
            kind: z.ZodLiteral<"verb_body">;
            member: z.ZodString;
            args: z.ZodEnum<["path_body", "path_options"]>;
            body_key: z.ZodOptional<z.ZodString>;
        }, "strip", z.ZodTypeAny, {
            kind: "verb_body";
            member: string;
            args: "path_body" | "path_options";
            body_key?: string | undefined;
        }, {
            kind: "verb_body";
            member: string;
            args: "path_body" | "path_options";
            body_key?: string | undefined;
        }>, z.ZodObject<{
            kind: z.ZodLiteral<"request">;
            member: z.ZodNullable<z.ZodString>;
            args: z.ZodEnum<["config", "path_options"]>;
            url_key: z.ZodOptional<z.ZodString>;
            method_key: z.ZodString;
        }, "strip", z.ZodTypeAny, {
            kind: "request";
            member: string | null;
            args: "path_options" | "config";
            method_key: string;
            url_key?: string | undefined;
        }, {
            kind: "request";
            member: string | null;
            args: "path_options" | "config";
            method_key: string;
            url_key?: string | undefined;
        }>, z.ZodObject<{
            kind: z.ZodLiteral<"request_body">;
            member: z.ZodNullable<z.ZodString>;
            args: z.ZodEnum<["config", "path_options"]>;
            url_key: z.ZodOptional<z.ZodString>;
            method_key: z.ZodString;
            body_key: z.ZodString;
        }, "strip", z.ZodTypeAny, {
            kind: "request_body";
            member: string | null;
            args: "path_options" | "config";
            body_key: string;
            method_key: string;
            url_key?: string | undefined;
        }, {
            kind: "request_body";
            member: string | null;
            args: "path_options" | "config";
            body_key: string;
            method_key: string;
            url_key?: string | undefined;
        }>]>;
    }, "strip", z.ZodTypeAny, {
        receiver: string;
        export: string;
        package: string;
        claim_id: string;
        claim: {
            kind: "factory";
            member: string;
            base_url_key: string;
        } | {
            kind: "verb";
            member: string;
            method: string;
        } | {
            kind: "verb_body";
            member: string;
            args: "path_body" | "path_options";
            body_key?: string | undefined;
        } | {
            kind: "request";
            member: string | null;
            args: "path_options" | "config";
            method_key: string;
            url_key?: string | undefined;
        } | {
            kind: "request_body";
            member: string | null;
            args: "path_options" | "config";
            body_key: string;
            method_key: string;
            url_key?: string | undefined;
        };
    }, {
        receiver: string;
        export: string;
        package: string;
        claim_id: string;
        claim: {
            kind: "factory";
            member: string;
            base_url_key: string;
        } | {
            kind: "verb";
            member: string;
            method: string;
        } | {
            kind: "verb_body";
            member: string;
            args: "path_body" | "path_options";
            body_key?: string | undefined;
        } | {
            kind: "request";
            member: string | null;
            args: "path_options" | "config";
            method_key: string;
            url_key?: string | undefined;
        } | {
            kind: "request_body";
            member: string | null;
            args: "path_options" | "config";
            body_key: string;
            method_key: string;
            url_key?: string | undefined;
        };
    }>, "many">;
    budget_ms: z.ZodOptional<z.ZodNumber>;
}, "strip", z.ZodTypeAny, {
    request_id: string;
    action: "verify_client_semantics";
    from_dir: string;
    checks: {
        receiver: string;
        export: string;
        package: string;
        claim_id: string;
        claim: {
            kind: "factory";
            member: string;
            base_url_key: string;
        } | {
            kind: "verb";
            member: string;
            method: string;
        } | {
            kind: "verb_body";
            member: string;
            args: "path_body" | "path_options";
            body_key?: string | undefined;
        } | {
            kind: "request";
            member: string | null;
            args: "path_options" | "config";
            method_key: string;
            url_key?: string | undefined;
        } | {
            kind: "request_body";
            member: string | null;
            args: "path_options" | "config";
            body_key: string;
            method_key: string;
            url_key?: string | undefined;
        };
    }[];
    budget_ms?: number | undefined;
}, {
    request_id: string;
    action: "verify_client_semantics";
    from_dir: string;
    checks: {
        receiver: string;
        export: string;
        package: string;
        claim_id: string;
        claim: {
            kind: "factory";
            member: string;
            base_url_key: string;
        } | {
            kind: "verb";
            member: string;
            method: string;
        } | {
            kind: "verb_body";
            member: string;
            args: "path_body" | "path_options";
            body_key?: string | undefined;
        } | {
            kind: "request";
            member: string | null;
            args: "path_options" | "config";
            method_key: string;
            url_key?: string | undefined;
        } | {
            kind: "request_body";
            member: string | null;
            args: "path_options" | "config";
            body_key: string;
            method_key: string;
            url_key?: string | undefined;
        };
    }[];
    budget_ms?: number | undefined;
}>, z.ZodObject<{
    request_id: z.ZodString;
} & {
    action: z.ZodLiteral<"verify_library_claims">;
    from_dir: z.ZodString;
    scan_root: z.ZodOptional<z.ZodString>;
    checks: z.ZodArray<z.ZodObject<{
        claim_id: z.ZodString;
        package: z.ZodString;
        export: z.ZodString;
        role: z.ZodEnum<["http_client", "graphql_client", "broker", "in_process_bus", "socket", "server_framework", "invoke_client", "none"]>;
        receiver: z.ZodString;
        claim: z.ZodDiscriminatedUnion<"kind", [z.ZodObject<{
            kind: z.ZodLiteral<"make">;
            form: z.ZodEnum<["call", "new"]>;
            member: z.ZodNullable<z.ZodString>;
            base: z.ZodOptional<z.ZodObject<{
                arg: z.ZodNumber;
                key: z.ZodOptional<z.ZodString>;
                at: z.ZodOptional<z.ZodArray<z.ZodString, "many">>;
                each: z.ZodOptional<z.ZodBoolean>;
                handle: z.ZodOptional<z.ZodBoolean>;
                or_file: z.ZodOptional<z.ZodBoolean>;
            }, "strict", z.ZodTypeAny, {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            }, {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            }>>;
            prefix: z.ZodOptional<z.ZodObject<{
                arg: z.ZodNumber;
                key: z.ZodOptional<z.ZodString>;
                at: z.ZodOptional<z.ZodArray<z.ZodString, "many">>;
                each: z.ZodOptional<z.ZodBoolean>;
                handle: z.ZodOptional<z.ZodBoolean>;
                or_file: z.ZodOptional<z.ZodBoolean>;
            }, "strict", z.ZodTypeAny, {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            }, {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            }>>;
            name: z.ZodOptional<z.ZodObject<{
                arg: z.ZodNumber;
                key: z.ZodOptional<z.ZodString>;
                at: z.ZodOptional<z.ZodArray<z.ZodString, "many">>;
                each: z.ZodOptional<z.ZodBoolean>;
                handle: z.ZodOptional<z.ZodBoolean>;
                or_file: z.ZodOptional<z.ZodBoolean>;
            } & {
                document: z.ZodOptional<z.ZodLiteral<"graphql">>;
                ref: z.ZodOptional<z.ZodEnum<["function", "class"]>>;
            }, "strict", z.ZodTypeAny, {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                document?: "graphql" | undefined;
                ref?: "function" | "class" | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            }, {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                document?: "graphql" | undefined;
                ref?: "function" | "class" | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            }>>;
            handler: z.ZodOptional<z.ZodObject<{
                arg: z.ZodNumber;
                key: z.ZodOptional<z.ZodString>;
                at: z.ZodOptional<z.ZodArray<z.ZodString, "many">>;
                each: z.ZodOptional<z.ZodBoolean>;
                handle: z.ZodOptional<z.ZodBoolean>;
                or_file: z.ZodOptional<z.ZodBoolean>;
            }, "strict", z.ZodTypeAny, {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            }, {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            }>>;
            runs: z.ZodOptional<z.ZodObject<{
                arg: z.ZodNumber;
                key: z.ZodOptional<z.ZodString>;
                at: z.ZodOptional<z.ZodArray<z.ZodString, "many">>;
                each: z.ZodOptional<z.ZodBoolean>;
                handle: z.ZodOptional<z.ZodBoolean>;
                or_file: z.ZodOptional<z.ZodBoolean>;
            }, "strict", z.ZodTypeAny, {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            }, {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            }>>;
            key_labels: z.ZodOptional<z.ZodRecord<z.ZodString, z.ZodEnum<["name", "not_name"]>>>;
            name_scope: z.ZodOptional<z.ZodObject<{
                scope: z.ZodEnum<["global", "service"]>;
                namespace: z.ZodNullable<z.ZodString>;
            }, "strict", z.ZodTypeAny, {
                scope: "global" | "service";
                namespace: string | null;
            }, {
                scope: "global" | "service";
                namespace: string | null;
            }>>;
            picker: z.ZodOptional<z.ZodString>;
            result: z.ZodOptional<z.ZodUnion<[z.ZodObject<{
                whole: z.ZodLiteral<true>;
            }, "strict", z.ZodTypeAny, {
                whole: true;
            }, {
                whole: true;
            }>, z.ZodObject<{
                member: z.ZodTuple<[z.ZodString], null>;
            }, "strict", z.ZodTypeAny, {
                member: [string];
            }, {
                member: [string];
            }>, z.ZodObject<{
                parse: z.ZodString;
            }, "strict", z.ZodTypeAny, {
                parse: string;
            }, {
                parse: string;
            }>]>>;
        }, "strict", z.ZodTypeAny, {
            kind: "make";
            member: string | null;
            form: "call" | "new";
            name?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                document?: "graphql" | undefined;
                ref?: "function" | "class" | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            result?: {
                whole: true;
            } | {
                member: [string];
            } | {
                parse: string;
            } | undefined;
            base?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            runs?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            prefix?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            handler?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            key_labels?: Record<string, "name" | "not_name"> | undefined;
            name_scope?: {
                scope: "global" | "service";
                namespace: string | null;
            } | undefined;
            picker?: string | undefined;
        }, {
            kind: "make";
            member: string | null;
            form: "call" | "new";
            name?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                document?: "graphql" | undefined;
                ref?: "function" | "class" | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            result?: {
                whole: true;
            } | {
                member: [string];
            } | {
                parse: string;
            } | undefined;
            base?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            runs?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            prefix?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            handler?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            key_labels?: Record<string, "name" | "not_name"> | undefined;
            name_scope?: {
                scope: "global" | "service";
                namespace: string | null;
            } | undefined;
            picker?: string | undefined;
        }>, z.ZodObject<{
            kind: z.ZodLiteral<"scope">;
            member: z.ZodString;
            form: z.ZodOptional<z.ZodEnum<["call", "decorate"]>>;
            name: z.ZodOptional<z.ZodObject<{
                arg: z.ZodNumber;
                key: z.ZodOptional<z.ZodString>;
                at: z.ZodOptional<z.ZodArray<z.ZodString, "many">>;
                each: z.ZodOptional<z.ZodBoolean>;
                handle: z.ZodOptional<z.ZodBoolean>;
                or_file: z.ZodOptional<z.ZodBoolean>;
            } & {
                document: z.ZodOptional<z.ZodLiteral<"graphql">>;
                ref: z.ZodOptional<z.ZodEnum<["function", "class"]>>;
            }, "strict", z.ZodTypeAny, {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                document?: "graphql" | undefined;
                ref?: "function" | "class" | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            }, {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                document?: "graphql" | undefined;
                ref?: "function" | "class" | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            }>>;
            path: z.ZodOptional<z.ZodArray<z.ZodString, "many">>;
            on: z.ZodOptional<z.ZodEnum<["export", "instance", "both"]>>;
            of: z.ZodOptional<z.ZodString>;
            key_labels: z.ZodOptional<z.ZodRecord<z.ZodString, z.ZodEnum<["name", "not_name"]>>>;
            name_scope: z.ZodOptional<z.ZodObject<{
                scope: z.ZodEnum<["global", "service"]>;
                namespace: z.ZodNullable<z.ZodString>;
            }, "strict", z.ZodTypeAny, {
                scope: "global" | "service";
                namespace: string | null;
            }, {
                scope: "global" | "service";
                namespace: string | null;
            }>>;
            picker: z.ZodOptional<z.ZodString>;
            runs: z.ZodOptional<z.ZodObject<{
                arg: z.ZodNumber;
                key: z.ZodOptional<z.ZodString>;
                at: z.ZodOptional<z.ZodArray<z.ZodString, "many">>;
                each: z.ZodOptional<z.ZodBoolean>;
                handle: z.ZodOptional<z.ZodBoolean>;
                or_file: z.ZodOptional<z.ZodBoolean>;
            }, "strict", z.ZodTypeAny, {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            }, {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            }>>;
            yields: z.ZodOptional<z.ZodObject<{
                param: z.ZodNumber;
                export: z.ZodString;
                side: z.ZodString;
                package: z.ZodOptional<z.ZodString>;
            }, "strict", z.ZodTypeAny, {
                side: string;
                export: string;
                param: number;
                package?: string | undefined;
            }, {
                side: string;
                export: string;
                param: number;
                package?: string | undefined;
            }>>;
            mounts: z.ZodOptional<z.ZodObject<{
                arg: z.ZodNumber;
                key: z.ZodOptional<z.ZodString>;
                at: z.ZodOptional<z.ZodArray<z.ZodString, "many">>;
                each: z.ZodOptional<z.ZodBoolean>;
                handle: z.ZodOptional<z.ZodBoolean>;
                or_file: z.ZodOptional<z.ZodBoolean>;
            }, "strict", z.ZodTypeAny, {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            }, {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            }>>;
            collects: z.ZodOptional<z.ZodObject<{
                arg: z.ZodNumber;
                key: z.ZodOptional<z.ZodString>;
                join: z.ZodString;
            }, "strict", z.ZodTypeAny, {
                join: string;
                arg: number;
                key?: string | undefined;
            }, {
                join: string;
                arg: number;
                key?: string | undefined;
            }>>;
            merges: z.ZodOptional<z.ZodObject<{
                arg: z.ZodNumber;
                key: z.ZodOptional<z.ZodString>;
                at: z.ZodOptional<z.ZodArray<z.ZodString, "many">>;
                each: z.ZodOptional<z.ZodBoolean>;
                handle: z.ZodOptional<z.ZodBoolean>;
                or_file: z.ZodOptional<z.ZodBoolean>;
            }, "strict", z.ZodTypeAny, {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            }, {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            }>>;
            passes: z.ZodOptional<z.ZodBoolean>;
        }, "strict", z.ZodTypeAny, {
            kind: "scope";
            member: string;
            name?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                document?: "graphql" | undefined;
                ref?: "function" | "class" | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            path?: string[] | undefined;
            runs?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            yields?: {
                side: string;
                export: string;
                param: number;
                package?: string | undefined;
            } | undefined;
            form?: "call" | "decorate" | undefined;
            key_labels?: Record<string, "name" | "not_name"> | undefined;
            name_scope?: {
                scope: "global" | "service";
                namespace: string | null;
            } | undefined;
            of?: string | undefined;
            on?: "both" | "export" | "instance" | undefined;
            mounts?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            merges?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            picker?: string | undefined;
            collects?: {
                join: string;
                arg: number;
                key?: string | undefined;
            } | undefined;
            passes?: boolean | undefined;
        }, {
            kind: "scope";
            member: string;
            name?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                document?: "graphql" | undefined;
                ref?: "function" | "class" | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            path?: string[] | undefined;
            runs?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            yields?: {
                side: string;
                export: string;
                param: number;
                package?: string | undefined;
            } | undefined;
            form?: "call" | "decorate" | undefined;
            key_labels?: Record<string, "name" | "not_name"> | undefined;
            name_scope?: {
                scope: "global" | "service";
                namespace: string | null;
            } | undefined;
            of?: string | undefined;
            on?: "both" | "export" | "instance" | undefined;
            mounts?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            merges?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            picker?: string | undefined;
            collects?: {
                join: string;
                arg: number;
                key?: string | undefined;
            } | undefined;
            passes?: boolean | undefined;
        }>, z.ZodObject<{
            kind: z.ZodLiteral<"op">;
            op: z.ZodEnum<["request", "send", "receive", "execute"]>;
            member: z.ZodNullable<z.ZodString>;
            path: z.ZodOptional<z.ZodArray<z.ZodString, "many">>;
            on: z.ZodOptional<z.ZodEnum<["export", "instance", "both"]>>;
            of: z.ZodOptional<z.ZodString>;
            form: z.ZodOptional<z.ZodEnum<["call", "new", "set", "override", "decorate"]>>;
            protocol: z.ZodOptional<z.ZodEnum<["http", "graphql", "rpc", "socket", "message"]>>;
            when: z.ZodOptional<z.ZodObject<{
                arg: z.ZodNumber;
                key: z.ZodOptional<z.ZodString>;
                literal: z.ZodString;
            }, "strict", z.ZodTypeAny, {
                literal: string;
                arg: number;
                key?: string | undefined;
            }, {
                literal: string;
                arg: number;
                key?: string | undefined;
            }>>;
            returns: z.ZodOptional<z.ZodObject<{
                result: z.ZodOptional<z.ZodUnion<[z.ZodObject<{
                    whole: z.ZodLiteral<true>;
                }, "strict", z.ZodTypeAny, {
                    whole: true;
                }, {
                    whole: true;
                }>, z.ZodObject<{
                    member: z.ZodTuple<[z.ZodString], null>;
                }, "strict", z.ZodTypeAny, {
                    member: [string];
                }, {
                    member: [string];
                }>, z.ZodObject<{
                    parse: z.ZodString;
                }, "strict", z.ZodTypeAny, {
                    parse: string;
                }, {
                    parse: string;
                }>]>>;
            }, "strict", z.ZodTypeAny, {
                result?: {
                    whole: true;
                } | {
                    member: [string];
                } | {
                    parse: string;
                } | undefined;
            }, {
                result?: {
                    whole: true;
                } | {
                    member: [string];
                } | {
                    parse: string;
                } | undefined;
            }>>;
            name: z.ZodOptional<z.ZodUnion<[z.ZodObject<{
                bound: z.ZodEnum<["maker", "scope", "key", "path", "export", "subscribed", "decorated"]>;
                join: z.ZodOptional<z.ZodString>;
                param: z.ZodOptional<z.ZodNumber>;
                at: z.ZodOptional<z.ZodArray<z.ZodString, "many">>;
            }, "strict", z.ZodTypeAny, {
                bound: "decorated" | "maker" | "scope" | "key" | "path" | "export" | "subscribed";
                join?: string | undefined;
                at?: string[] | undefined;
                param?: number | undefined;
            }, {
                bound: "decorated" | "maker" | "scope" | "key" | "path" | "export" | "subscribed";
                join?: string | undefined;
                at?: string[] | undefined;
                param?: number | undefined;
            }>, z.ZodObject<{
                parts: z.ZodArray<z.ZodObject<{
                    arg: z.ZodNumber;
                    key: z.ZodOptional<z.ZodString>;
                    at: z.ZodOptional<z.ZodArray<z.ZodString, "many">>;
                    each: z.ZodOptional<z.ZodBoolean>;
                    handle: z.ZodOptional<z.ZodBoolean>;
                    or_file: z.ZodOptional<z.ZodBoolean>;
                }, "strict", z.ZodTypeAny, {
                    arg: number;
                    at?: string[] | undefined;
                    key?: string | undefined;
                    each?: boolean | undefined;
                    handle?: boolean | undefined;
                    or_file?: boolean | undefined;
                }, {
                    arg: number;
                    at?: string[] | undefined;
                    key?: string | undefined;
                    each?: boolean | undefined;
                    handle?: boolean | undefined;
                    or_file?: boolean | undefined;
                }>, "many">;
                join: z.ZodOptional<z.ZodString>;
            }, "strict", z.ZodTypeAny, {
                parts: {
                    arg: number;
                    at?: string[] | undefined;
                    key?: string | undefined;
                    each?: boolean | undefined;
                    handle?: boolean | undefined;
                    or_file?: boolean | undefined;
                }[];
                join?: string | undefined;
            }, {
                parts: {
                    arg: number;
                    at?: string[] | undefined;
                    key?: string | undefined;
                    each?: boolean | undefined;
                    handle?: boolean | undefined;
                    or_file?: boolean | undefined;
                }[];
                join?: string | undefined;
            }>, z.ZodObject<{
                literal: z.ZodString;
            }, "strict", z.ZodTypeAny, {
                literal: string;
            }, {
                literal: string;
            }>, z.ZodObject<{
                first: z.ZodArray<z.ZodUnion<[z.ZodObject<{
                    arg: z.ZodNumber;
                    key: z.ZodOptional<z.ZodString>;
                }, "strict", z.ZodTypeAny, {
                    arg: number;
                    key?: string | undefined;
                }, {
                    arg: number;
                    key?: string | undefined;
                }>, z.ZodObject<{
                    bound: z.ZodLiteral<"decorated">;
                }, "strict", z.ZodTypeAny, {
                    bound: "decorated";
                }, {
                    bound: "decorated";
                }>]>, "many">;
            }, "strict", z.ZodTypeAny, {
                first: ({
                    arg: number;
                    key?: string | undefined;
                } | {
                    bound: "decorated";
                })[];
            }, {
                first: ({
                    arg: number;
                    key?: string | undefined;
                } | {
                    bound: "decorated";
                })[];
            }>, z.ZodObject<{
                arg: z.ZodNumber;
                key: z.ZodOptional<z.ZodString>;
                at: z.ZodOptional<z.ZodArray<z.ZodString, "many">>;
                each: z.ZodOptional<z.ZodBoolean>;
                handle: z.ZodOptional<z.ZodBoolean>;
                or_file: z.ZodOptional<z.ZodBoolean>;
            } & {
                document: z.ZodOptional<z.ZodLiteral<"graphql">>;
                ref: z.ZodOptional<z.ZodEnum<["function", "class"]>>;
            }, "strict", z.ZodTypeAny, {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                document?: "graphql" | undefined;
                ref?: "function" | "class" | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            }, {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                document?: "graphql" | undefined;
                ref?: "function" | "class" | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            }>]>>;
            payload: z.ZodOptional<z.ZodObject<{
                arg: z.ZodNumber;
                key: z.ZodOptional<z.ZodString>;
                at: z.ZodOptional<z.ZodArray<z.ZodString, "many">>;
                each: z.ZodOptional<z.ZodBoolean>;
                handle: z.ZodOptional<z.ZodBoolean>;
                or_file: z.ZodOptional<z.ZodBoolean>;
            }, "strict", z.ZodTypeAny, {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            }, {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            }>>;
            handler: z.ZodOptional<z.ZodObject<{
                arg: z.ZodNumber;
                key: z.ZodOptional<z.ZodString>;
                at: z.ZodOptional<z.ZodArray<z.ZodString, "many">>;
                each: z.ZodOptional<z.ZodBoolean>;
                handle: z.ZodOptional<z.ZodBoolean>;
                or_file: z.ZodOptional<z.ZodBoolean>;
            }, "strict", z.ZodTypeAny, {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            }, {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            }>>;
            ack: z.ZodOptional<z.ZodObject<{
                arg: z.ZodNumber;
                key: z.ZodOptional<z.ZodString>;
                at: z.ZodOptional<z.ZodArray<z.ZodString, "many">>;
                each: z.ZodOptional<z.ZodBoolean>;
                handle: z.ZodOptional<z.ZodBoolean>;
                or_file: z.ZodOptional<z.ZodBoolean>;
            }, "strict", z.ZodTypeAny, {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            }, {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            }>>;
            key_labels: z.ZodOptional<z.ZodRecord<z.ZodString, z.ZodEnum<["name", "not_name"]>>>;
            name_scope: z.ZodOptional<z.ZodObject<{
                scope: z.ZodEnum<["global", "service"]>;
                namespace: z.ZodNullable<z.ZodString>;
            }, "strict", z.ZodTypeAny, {
                scope: "global" | "service";
                namespace: string | null;
            }, {
                scope: "global" | "service";
                namespace: string | null;
            }>>;
            picker: z.ZodOptional<z.ZodString>;
            options: z.ZodOptional<z.ZodObject<{
                arg: z.ZodNumber;
                key: z.ZodOptional<z.ZodString>;
                at: z.ZodOptional<z.ZodArray<z.ZodString, "many">>;
                each: z.ZodOptional<z.ZodBoolean>;
                handle: z.ZodOptional<z.ZodBoolean>;
                or_file: z.ZodOptional<z.ZodBoolean>;
            }, "strict", z.ZodTypeAny, {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            }, {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            }>>;
            method: z.ZodOptional<z.ZodString>;
            method_key: z.ZodOptional<z.ZodObject<{
                arg: z.ZodNumber;
                key: z.ZodOptional<z.ZodString>;
                at: z.ZodOptional<z.ZodArray<z.ZodString, "many">>;
                each: z.ZodOptional<z.ZodBoolean>;
                handle: z.ZodOptional<z.ZodBoolean>;
                or_file: z.ZodOptional<z.ZodBoolean>;
            }, "strict", z.ZodTypeAny, {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            }, {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            }>>;
            method_default: z.ZodOptional<z.ZodString>;
            base: z.ZodOptional<z.ZodObject<{
                arg: z.ZodNumber;
                key: z.ZodOptional<z.ZodString>;
                at: z.ZodOptional<z.ZodArray<z.ZodString, "many">>;
                each: z.ZodOptional<z.ZodBoolean>;
                handle: z.ZodOptional<z.ZodBoolean>;
                or_file: z.ZodOptional<z.ZodBoolean>;
            }, "strict", z.ZodTypeAny, {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            }, {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            }>>;
            runs: z.ZodOptional<z.ZodObject<{
                arg: z.ZodNumber;
                key: z.ZodOptional<z.ZodString>;
                bound: z.ZodOptional<z.ZodLiteral<"maker">>;
            }, "strict", z.ZodTypeAny, {
                arg: number;
                key?: string | undefined;
                bound?: "maker" | undefined;
            }, {
                arg: number;
                key?: string | undefined;
                bound?: "maker" | undefined;
            }>>;
            yields: z.ZodOptional<z.ZodObject<{
                param: z.ZodNumber;
                export: z.ZodString;
                side: z.ZodString;
                package: z.ZodOptional<z.ZodString>;
            }, "strict", z.ZodTypeAny, {
                side: string;
                export: string;
                param: number;
                package?: string | undefined;
            }, {
                side: string;
                export: string;
                param: number;
                package?: string | undefined;
            }>>;
            request: z.ZodOptional<z.ZodObject<{
                param: z.ZodNumber;
                path: z.ZodOptional<z.ZodArray<z.ZodString, "many">>;
                url: z.ZodOptional<z.ZodArray<z.ZodString, "many">>;
                method: z.ZodOptional<z.ZodArray<z.ZodString, "many">>;
                body: z.ZodOptional<z.ZodArray<z.ZodString, "many">>;
            }, "strict", z.ZodTypeAny, {
                param: number;
                body?: string[] | undefined;
                url?: string[] | undefined;
                path?: string[] | undefined;
                method?: string[] | undefined;
            }, {
                param: number;
                body?: string[] | undefined;
                url?: string[] | undefined;
                path?: string[] | undefined;
                method?: string[] | undefined;
            }>>;
            result: z.ZodOptional<z.ZodUnion<[z.ZodObject<{
                whole: z.ZodLiteral<true>;
            }, "strict", z.ZodTypeAny, {
                whole: true;
            }, {
                whole: true;
            }>, z.ZodObject<{
                member: z.ZodTuple<[z.ZodString], null>;
            }, "strict", z.ZodTypeAny, {
                member: [string];
            }, {
                member: [string];
            }>, z.ZodObject<{
                parse: z.ZodString;
            }, "strict", z.ZodTypeAny, {
                parse: string;
            }, {
                parse: string;
            }>]>>;
        }, "strict", z.ZodTypeAny, {
            kind: "op";
            member: string | null;
            op: "request" | "send" | "receive" | "execute";
            name?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                document?: "graphql" | undefined;
                ref?: "function" | "class" | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | {
                parts: {
                    arg: number;
                    at?: string[] | undefined;
                    key?: string | undefined;
                    each?: boolean | undefined;
                    handle?: boolean | undefined;
                    or_file?: boolean | undefined;
                }[];
                join?: string | undefined;
            } | {
                literal: string;
            } | {
                bound: "decorated" | "maker" | "scope" | "key" | "path" | "export" | "subscribed";
                join?: string | undefined;
                at?: string[] | undefined;
                param?: number | undefined;
            } | {
                first: ({
                    arg: number;
                    key?: string | undefined;
                } | {
                    bound: "decorated";
                })[];
            } | undefined;
            request?: {
                param: number;
                body?: string[] | undefined;
                url?: string[] | undefined;
                path?: string[] | undefined;
                method?: string[] | undefined;
            } | undefined;
            path?: string[] | undefined;
            result?: {
                whole: true;
            } | {
                member: [string];
            } | {
                parse: string;
            } | undefined;
            method_default?: string | undefined;
            base?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            runs?: {
                arg: number;
                key?: string | undefined;
                bound?: "maker" | undefined;
            } | undefined;
            yields?: {
                side: string;
                export: string;
                param: number;
                package?: string | undefined;
            } | undefined;
            form?: "call" | "new" | "set" | "override" | "decorate" | undefined;
            handler?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            key_labels?: Record<string, "name" | "not_name"> | undefined;
            name_scope?: {
                scope: "global" | "service";
                namespace: string | null;
            } | undefined;
            method?: string | undefined;
            payload?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            options?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            method_key?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            returns?: {
                result?: {
                    whole: true;
                } | {
                    member: [string];
                } | {
                    parse: string;
                } | undefined;
            } | undefined;
            when?: {
                literal: string;
                arg: number;
                key?: string | undefined;
            } | undefined;
            of?: string | undefined;
            on?: "both" | "export" | "instance" | undefined;
            ack?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            protocol?: "http" | "graphql" | "socket" | "rpc" | "message" | undefined;
            picker?: string | undefined;
        }, {
            kind: "op";
            member: string | null;
            op: "request" | "send" | "receive" | "execute";
            name?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                document?: "graphql" | undefined;
                ref?: "function" | "class" | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | {
                parts: {
                    arg: number;
                    at?: string[] | undefined;
                    key?: string | undefined;
                    each?: boolean | undefined;
                    handle?: boolean | undefined;
                    or_file?: boolean | undefined;
                }[];
                join?: string | undefined;
            } | {
                literal: string;
            } | {
                bound: "decorated" | "maker" | "scope" | "key" | "path" | "export" | "subscribed";
                join?: string | undefined;
                at?: string[] | undefined;
                param?: number | undefined;
            } | {
                first: ({
                    arg: number;
                    key?: string | undefined;
                } | {
                    bound: "decorated";
                })[];
            } | undefined;
            request?: {
                param: number;
                body?: string[] | undefined;
                url?: string[] | undefined;
                path?: string[] | undefined;
                method?: string[] | undefined;
            } | undefined;
            path?: string[] | undefined;
            result?: {
                whole: true;
            } | {
                member: [string];
            } | {
                parse: string;
            } | undefined;
            method_default?: string | undefined;
            base?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            runs?: {
                arg: number;
                key?: string | undefined;
                bound?: "maker" | undefined;
            } | undefined;
            yields?: {
                side: string;
                export: string;
                param: number;
                package?: string | undefined;
            } | undefined;
            form?: "call" | "new" | "set" | "override" | "decorate" | undefined;
            handler?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            key_labels?: Record<string, "name" | "not_name"> | undefined;
            name_scope?: {
                scope: "global" | "service";
                namespace: string | null;
            } | undefined;
            method?: string | undefined;
            payload?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            options?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            method_key?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            returns?: {
                result?: {
                    whole: true;
                } | {
                    member: [string];
                } | {
                    parse: string;
                } | undefined;
            } | undefined;
            when?: {
                literal: string;
                arg: number;
                key?: string | undefined;
            } | undefined;
            of?: string | undefined;
            on?: "both" | "export" | "instance" | undefined;
            ack?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            protocol?: "http" | "graphql" | "socket" | "rpc" | "message" | undefined;
            picker?: string | undefined;
        }>, z.ZodObject<{
            kind: z.ZodLiteral<"result">;
            form: z.ZodEnum<["make", "op"]>;
            member: z.ZodNullable<z.ZodString>;
            path: z.ZodOptional<z.ZodArray<z.ZodString, "many">>;
            on: z.ZodOptional<z.ZodEnum<["export", "instance", "both"]>>;
            of: z.ZodOptional<z.ZodString>;
            picker: z.ZodOptional<z.ZodString>;
            result: z.ZodUnion<[z.ZodObject<{
                whole: z.ZodLiteral<true>;
            }, "strict", z.ZodTypeAny, {
                whole: true;
            }, {
                whole: true;
            }>, z.ZodObject<{
                member: z.ZodTuple<[z.ZodString], null>;
            }, "strict", z.ZodTypeAny, {
                member: [string];
            }, {
                member: [string];
            }>, z.ZodObject<{
                parse: z.ZodString;
            }, "strict", z.ZodTypeAny, {
                parse: string;
            }, {
                parse: string;
            }>]>;
        }, "strict", z.ZodTypeAny, {
            kind: "result";
            member: string | null;
            result: {
                whole: true;
            } | {
                member: [string];
            } | {
                parse: string;
            };
            form: "make" | "op";
            path?: string[] | undefined;
            of?: string | undefined;
            on?: "both" | "export" | "instance" | undefined;
            picker?: string | undefined;
        }, {
            kind: "result";
            member: string | null;
            result: {
                whole: true;
            } | {
                member: [string];
            } | {
                parse: string;
            };
            form: "make" | "op";
            path?: string[] | undefined;
            of?: string | undefined;
            on?: "both" | "export" | "instance" | undefined;
            picker?: string | undefined;
        }>, z.ZodObject<{
            kind: z.ZodLiteral<"reserved">;
            member: z.ZodString;
            name: z.ZodString;
            path: z.ZodOptional<z.ZodArray<z.ZodString, "many">>;
            on: z.ZodOptional<z.ZodEnum<["export", "instance", "both"]>>;
            of: z.ZodOptional<z.ZodString>;
            picker: z.ZodOptional<z.ZodString>;
        }, "strict", z.ZodTypeAny, {
            kind: "reserved";
            name: string;
            member: string;
            path?: string[] | undefined;
            of?: string | undefined;
            on?: "both" | "export" | "instance" | undefined;
            picker?: string | undefined;
        }, {
            kind: "reserved";
            name: string;
            member: string;
            path?: string[] | undefined;
            of?: string | undefined;
            on?: "both" | "export" | "instance" | undefined;
            picker?: string | undefined;
        }>]>;
    }, "strict", z.ZodTypeAny, {
        receiver: string;
        export: string;
        package: string;
        claim_id: string;
        claim: {
            kind: "make";
            member: string | null;
            form: "call" | "new";
            name?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                document?: "graphql" | undefined;
                ref?: "function" | "class" | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            result?: {
                whole: true;
            } | {
                member: [string];
            } | {
                parse: string;
            } | undefined;
            base?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            runs?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            prefix?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            handler?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            key_labels?: Record<string, "name" | "not_name"> | undefined;
            name_scope?: {
                scope: "global" | "service";
                namespace: string | null;
            } | undefined;
            picker?: string | undefined;
        } | {
            kind: "scope";
            member: string;
            name?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                document?: "graphql" | undefined;
                ref?: "function" | "class" | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            path?: string[] | undefined;
            runs?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            yields?: {
                side: string;
                export: string;
                param: number;
                package?: string | undefined;
            } | undefined;
            form?: "call" | "decorate" | undefined;
            key_labels?: Record<string, "name" | "not_name"> | undefined;
            name_scope?: {
                scope: "global" | "service";
                namespace: string | null;
            } | undefined;
            of?: string | undefined;
            on?: "both" | "export" | "instance" | undefined;
            mounts?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            merges?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            picker?: string | undefined;
            collects?: {
                join: string;
                arg: number;
                key?: string | undefined;
            } | undefined;
            passes?: boolean | undefined;
        } | {
            kind: "op";
            member: string | null;
            op: "request" | "send" | "receive" | "execute";
            name?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                document?: "graphql" | undefined;
                ref?: "function" | "class" | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | {
                parts: {
                    arg: number;
                    at?: string[] | undefined;
                    key?: string | undefined;
                    each?: boolean | undefined;
                    handle?: boolean | undefined;
                    or_file?: boolean | undefined;
                }[];
                join?: string | undefined;
            } | {
                literal: string;
            } | {
                bound: "decorated" | "maker" | "scope" | "key" | "path" | "export" | "subscribed";
                join?: string | undefined;
                at?: string[] | undefined;
                param?: number | undefined;
            } | {
                first: ({
                    arg: number;
                    key?: string | undefined;
                } | {
                    bound: "decorated";
                })[];
            } | undefined;
            request?: {
                param: number;
                body?: string[] | undefined;
                url?: string[] | undefined;
                path?: string[] | undefined;
                method?: string[] | undefined;
            } | undefined;
            path?: string[] | undefined;
            result?: {
                whole: true;
            } | {
                member: [string];
            } | {
                parse: string;
            } | undefined;
            method_default?: string | undefined;
            base?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            runs?: {
                arg: number;
                key?: string | undefined;
                bound?: "maker" | undefined;
            } | undefined;
            yields?: {
                side: string;
                export: string;
                param: number;
                package?: string | undefined;
            } | undefined;
            form?: "call" | "new" | "set" | "override" | "decorate" | undefined;
            handler?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            key_labels?: Record<string, "name" | "not_name"> | undefined;
            name_scope?: {
                scope: "global" | "service";
                namespace: string | null;
            } | undefined;
            method?: string | undefined;
            payload?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            options?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            method_key?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            returns?: {
                result?: {
                    whole: true;
                } | {
                    member: [string];
                } | {
                    parse: string;
                } | undefined;
            } | undefined;
            when?: {
                literal: string;
                arg: number;
                key?: string | undefined;
            } | undefined;
            of?: string | undefined;
            on?: "both" | "export" | "instance" | undefined;
            ack?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            protocol?: "http" | "graphql" | "socket" | "rpc" | "message" | undefined;
            picker?: string | undefined;
        } | {
            kind: "result";
            member: string | null;
            result: {
                whole: true;
            } | {
                member: [string];
            } | {
                parse: string;
            };
            form: "make" | "op";
            path?: string[] | undefined;
            of?: string | undefined;
            on?: "both" | "export" | "instance" | undefined;
            picker?: string | undefined;
        } | {
            kind: "reserved";
            name: string;
            member: string;
            path?: string[] | undefined;
            of?: string | undefined;
            on?: "both" | "export" | "instance" | undefined;
            picker?: string | undefined;
        };
        role: "none" | "socket" | "http_client" | "graphql_client" | "broker" | "in_process_bus" | "server_framework" | "invoke_client";
    }, {
        receiver: string;
        export: string;
        package: string;
        claim_id: string;
        claim: {
            kind: "make";
            member: string | null;
            form: "call" | "new";
            name?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                document?: "graphql" | undefined;
                ref?: "function" | "class" | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            result?: {
                whole: true;
            } | {
                member: [string];
            } | {
                parse: string;
            } | undefined;
            base?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            runs?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            prefix?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            handler?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            key_labels?: Record<string, "name" | "not_name"> | undefined;
            name_scope?: {
                scope: "global" | "service";
                namespace: string | null;
            } | undefined;
            picker?: string | undefined;
        } | {
            kind: "scope";
            member: string;
            name?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                document?: "graphql" | undefined;
                ref?: "function" | "class" | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            path?: string[] | undefined;
            runs?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            yields?: {
                side: string;
                export: string;
                param: number;
                package?: string | undefined;
            } | undefined;
            form?: "call" | "decorate" | undefined;
            key_labels?: Record<string, "name" | "not_name"> | undefined;
            name_scope?: {
                scope: "global" | "service";
                namespace: string | null;
            } | undefined;
            of?: string | undefined;
            on?: "both" | "export" | "instance" | undefined;
            mounts?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            merges?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            picker?: string | undefined;
            collects?: {
                join: string;
                arg: number;
                key?: string | undefined;
            } | undefined;
            passes?: boolean | undefined;
        } | {
            kind: "op";
            member: string | null;
            op: "request" | "send" | "receive" | "execute";
            name?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                document?: "graphql" | undefined;
                ref?: "function" | "class" | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | {
                parts: {
                    arg: number;
                    at?: string[] | undefined;
                    key?: string | undefined;
                    each?: boolean | undefined;
                    handle?: boolean | undefined;
                    or_file?: boolean | undefined;
                }[];
                join?: string | undefined;
            } | {
                literal: string;
            } | {
                bound: "decorated" | "maker" | "scope" | "key" | "path" | "export" | "subscribed";
                join?: string | undefined;
                at?: string[] | undefined;
                param?: number | undefined;
            } | {
                first: ({
                    arg: number;
                    key?: string | undefined;
                } | {
                    bound: "decorated";
                })[];
            } | undefined;
            request?: {
                param: number;
                body?: string[] | undefined;
                url?: string[] | undefined;
                path?: string[] | undefined;
                method?: string[] | undefined;
            } | undefined;
            path?: string[] | undefined;
            result?: {
                whole: true;
            } | {
                member: [string];
            } | {
                parse: string;
            } | undefined;
            method_default?: string | undefined;
            base?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            runs?: {
                arg: number;
                key?: string | undefined;
                bound?: "maker" | undefined;
            } | undefined;
            yields?: {
                side: string;
                export: string;
                param: number;
                package?: string | undefined;
            } | undefined;
            form?: "call" | "new" | "set" | "override" | "decorate" | undefined;
            handler?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            key_labels?: Record<string, "name" | "not_name"> | undefined;
            name_scope?: {
                scope: "global" | "service";
                namespace: string | null;
            } | undefined;
            method?: string | undefined;
            payload?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            options?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            method_key?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            returns?: {
                result?: {
                    whole: true;
                } | {
                    member: [string];
                } | {
                    parse: string;
                } | undefined;
            } | undefined;
            when?: {
                literal: string;
                arg: number;
                key?: string | undefined;
            } | undefined;
            of?: string | undefined;
            on?: "both" | "export" | "instance" | undefined;
            ack?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            protocol?: "http" | "graphql" | "socket" | "rpc" | "message" | undefined;
            picker?: string | undefined;
        } | {
            kind: "result";
            member: string | null;
            result: {
                whole: true;
            } | {
                member: [string];
            } | {
                parse: string;
            };
            form: "make" | "op";
            path?: string[] | undefined;
            of?: string | undefined;
            on?: "both" | "export" | "instance" | undefined;
            picker?: string | undefined;
        } | {
            kind: "reserved";
            name: string;
            member: string;
            path?: string[] | undefined;
            of?: string | undefined;
            on?: "both" | "export" | "instance" | undefined;
            picker?: string | undefined;
        };
        role: "none" | "socket" | "http_client" | "graphql_client" | "broker" | "in_process_bus" | "server_framework" | "invoke_client";
    }>, "many">;
    budget_ms: z.ZodOptional<z.ZodNullable<z.ZodNumber>>;
}, "strip", z.ZodTypeAny, {
    request_id: string;
    action: "verify_library_claims";
    from_dir: string;
    checks: {
        receiver: string;
        export: string;
        package: string;
        claim_id: string;
        claim: {
            kind: "make";
            member: string | null;
            form: "call" | "new";
            name?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                document?: "graphql" | undefined;
                ref?: "function" | "class" | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            result?: {
                whole: true;
            } | {
                member: [string];
            } | {
                parse: string;
            } | undefined;
            base?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            runs?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            prefix?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            handler?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            key_labels?: Record<string, "name" | "not_name"> | undefined;
            name_scope?: {
                scope: "global" | "service";
                namespace: string | null;
            } | undefined;
            picker?: string | undefined;
        } | {
            kind: "scope";
            member: string;
            name?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                document?: "graphql" | undefined;
                ref?: "function" | "class" | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            path?: string[] | undefined;
            runs?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            yields?: {
                side: string;
                export: string;
                param: number;
                package?: string | undefined;
            } | undefined;
            form?: "call" | "decorate" | undefined;
            key_labels?: Record<string, "name" | "not_name"> | undefined;
            name_scope?: {
                scope: "global" | "service";
                namespace: string | null;
            } | undefined;
            of?: string | undefined;
            on?: "both" | "export" | "instance" | undefined;
            mounts?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            merges?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            picker?: string | undefined;
            collects?: {
                join: string;
                arg: number;
                key?: string | undefined;
            } | undefined;
            passes?: boolean | undefined;
        } | {
            kind: "op";
            member: string | null;
            op: "request" | "send" | "receive" | "execute";
            name?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                document?: "graphql" | undefined;
                ref?: "function" | "class" | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | {
                parts: {
                    arg: number;
                    at?: string[] | undefined;
                    key?: string | undefined;
                    each?: boolean | undefined;
                    handle?: boolean | undefined;
                    or_file?: boolean | undefined;
                }[];
                join?: string | undefined;
            } | {
                literal: string;
            } | {
                bound: "decorated" | "maker" | "scope" | "key" | "path" | "export" | "subscribed";
                join?: string | undefined;
                at?: string[] | undefined;
                param?: number | undefined;
            } | {
                first: ({
                    arg: number;
                    key?: string | undefined;
                } | {
                    bound: "decorated";
                })[];
            } | undefined;
            request?: {
                param: number;
                body?: string[] | undefined;
                url?: string[] | undefined;
                path?: string[] | undefined;
                method?: string[] | undefined;
            } | undefined;
            path?: string[] | undefined;
            result?: {
                whole: true;
            } | {
                member: [string];
            } | {
                parse: string;
            } | undefined;
            method_default?: string | undefined;
            base?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            runs?: {
                arg: number;
                key?: string | undefined;
                bound?: "maker" | undefined;
            } | undefined;
            yields?: {
                side: string;
                export: string;
                param: number;
                package?: string | undefined;
            } | undefined;
            form?: "call" | "new" | "set" | "override" | "decorate" | undefined;
            handler?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            key_labels?: Record<string, "name" | "not_name"> | undefined;
            name_scope?: {
                scope: "global" | "service";
                namespace: string | null;
            } | undefined;
            method?: string | undefined;
            payload?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            options?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            method_key?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            returns?: {
                result?: {
                    whole: true;
                } | {
                    member: [string];
                } | {
                    parse: string;
                } | undefined;
            } | undefined;
            when?: {
                literal: string;
                arg: number;
                key?: string | undefined;
            } | undefined;
            of?: string | undefined;
            on?: "both" | "export" | "instance" | undefined;
            ack?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            protocol?: "http" | "graphql" | "socket" | "rpc" | "message" | undefined;
            picker?: string | undefined;
        } | {
            kind: "result";
            member: string | null;
            result: {
                whole: true;
            } | {
                member: [string];
            } | {
                parse: string;
            };
            form: "make" | "op";
            path?: string[] | undefined;
            of?: string | undefined;
            on?: "both" | "export" | "instance" | undefined;
            picker?: string | undefined;
        } | {
            kind: "reserved";
            name: string;
            member: string;
            path?: string[] | undefined;
            of?: string | undefined;
            on?: "both" | "export" | "instance" | undefined;
            picker?: string | undefined;
        };
        role: "none" | "socket" | "http_client" | "graphql_client" | "broker" | "in_process_bus" | "server_framework" | "invoke_client";
    }[];
    scan_root?: string | undefined;
    budget_ms?: number | null | undefined;
}, {
    request_id: string;
    action: "verify_library_claims";
    from_dir: string;
    checks: {
        receiver: string;
        export: string;
        package: string;
        claim_id: string;
        claim: {
            kind: "make";
            member: string | null;
            form: "call" | "new";
            name?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                document?: "graphql" | undefined;
                ref?: "function" | "class" | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            result?: {
                whole: true;
            } | {
                member: [string];
            } | {
                parse: string;
            } | undefined;
            base?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            runs?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            prefix?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            handler?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            key_labels?: Record<string, "name" | "not_name"> | undefined;
            name_scope?: {
                scope: "global" | "service";
                namespace: string | null;
            } | undefined;
            picker?: string | undefined;
        } | {
            kind: "scope";
            member: string;
            name?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                document?: "graphql" | undefined;
                ref?: "function" | "class" | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            path?: string[] | undefined;
            runs?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            yields?: {
                side: string;
                export: string;
                param: number;
                package?: string | undefined;
            } | undefined;
            form?: "call" | "decorate" | undefined;
            key_labels?: Record<string, "name" | "not_name"> | undefined;
            name_scope?: {
                scope: "global" | "service";
                namespace: string | null;
            } | undefined;
            of?: string | undefined;
            on?: "both" | "export" | "instance" | undefined;
            mounts?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            merges?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            picker?: string | undefined;
            collects?: {
                join: string;
                arg: number;
                key?: string | undefined;
            } | undefined;
            passes?: boolean | undefined;
        } | {
            kind: "op";
            member: string | null;
            op: "request" | "send" | "receive" | "execute";
            name?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                document?: "graphql" | undefined;
                ref?: "function" | "class" | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | {
                parts: {
                    arg: number;
                    at?: string[] | undefined;
                    key?: string | undefined;
                    each?: boolean | undefined;
                    handle?: boolean | undefined;
                    or_file?: boolean | undefined;
                }[];
                join?: string | undefined;
            } | {
                literal: string;
            } | {
                bound: "decorated" | "maker" | "scope" | "key" | "path" | "export" | "subscribed";
                join?: string | undefined;
                at?: string[] | undefined;
                param?: number | undefined;
            } | {
                first: ({
                    arg: number;
                    key?: string | undefined;
                } | {
                    bound: "decorated";
                })[];
            } | undefined;
            request?: {
                param: number;
                body?: string[] | undefined;
                url?: string[] | undefined;
                path?: string[] | undefined;
                method?: string[] | undefined;
            } | undefined;
            path?: string[] | undefined;
            result?: {
                whole: true;
            } | {
                member: [string];
            } | {
                parse: string;
            } | undefined;
            method_default?: string | undefined;
            base?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            runs?: {
                arg: number;
                key?: string | undefined;
                bound?: "maker" | undefined;
            } | undefined;
            yields?: {
                side: string;
                export: string;
                param: number;
                package?: string | undefined;
            } | undefined;
            form?: "call" | "new" | "set" | "override" | "decorate" | undefined;
            handler?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            key_labels?: Record<string, "name" | "not_name"> | undefined;
            name_scope?: {
                scope: "global" | "service";
                namespace: string | null;
            } | undefined;
            method?: string | undefined;
            payload?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            options?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            method_key?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            returns?: {
                result?: {
                    whole: true;
                } | {
                    member: [string];
                } | {
                    parse: string;
                } | undefined;
            } | undefined;
            when?: {
                literal: string;
                arg: number;
                key?: string | undefined;
            } | undefined;
            of?: string | undefined;
            on?: "both" | "export" | "instance" | undefined;
            ack?: {
                arg: number;
                at?: string[] | undefined;
                key?: string | undefined;
                each?: boolean | undefined;
                handle?: boolean | undefined;
                or_file?: boolean | undefined;
            } | undefined;
            protocol?: "http" | "graphql" | "socket" | "rpc" | "message" | undefined;
            picker?: string | undefined;
        } | {
            kind: "result";
            member: string | null;
            result: {
                whole: true;
            } | {
                member: [string];
            } | {
                parse: string;
            };
            form: "make" | "op";
            path?: string[] | undefined;
            of?: string | undefined;
            on?: "both" | "export" | "instance" | undefined;
            picker?: string | undefined;
        } | {
            kind: "reserved";
            name: string;
            member: string;
            path?: string[] | undefined;
            of?: string | undefined;
            on?: "both" | "export" | "instance" | undefined;
            picker?: string | undefined;
        };
        role: "none" | "socket" | "http_client" | "graphql_client" | "broker" | "in_process_bus" | "server_framework" | "invoke_client";
    }[];
    scan_root?: string | undefined;
    budget_ms?: number | null | undefined;
}>, z.ZodObject<{
    request_id: z.ZodString;
} & {
    action: z.ZodLiteral<"list_library_surface">;
    from_dir: z.ZodString;
    packages: z.ZodArray<z.ZodString, "many">;
    max_entries: z.ZodOptional<z.ZodNumber>;
    exports: z.ZodOptional<z.ZodRecord<z.ZodString, z.ZodArray<z.ZodString, "many">>>;
    handlers: z.ZodOptional<z.ZodBoolean>;
    types: z.ZodOptional<z.ZodBoolean>;
    result_candidates: z.ZodOptional<z.ZodBoolean>;
    max_handler_members: z.ZodOptional<z.ZodNumber>;
    foreign_maker_budget: z.ZodOptional<z.ZodNumber>;
}, "strip", z.ZodTypeAny, {
    packages: string[];
    request_id: string;
    action: "list_library_surface";
    from_dir: string;
    types?: boolean | undefined;
    handlers?: boolean | undefined;
    result_candidates?: boolean | undefined;
    exports?: Record<string, string[]> | undefined;
    max_entries?: number | undefined;
    max_handler_members?: number | undefined;
    foreign_maker_budget?: number | undefined;
}, {
    packages: string[];
    request_id: string;
    action: "list_library_surface";
    from_dir: string;
    types?: boolean | undefined;
    handlers?: boolean | undefined;
    result_candidates?: boolean | undefined;
    exports?: Record<string, string[]> | undefined;
    max_entries?: number | undefined;
    max_handler_members?: number | undefined;
    foreign_maker_budget?: number | undefined;
}>, z.ZodObject<{
    request_id: z.ZodString;
} & {
    action: z.ZodLiteral<"health">;
}, "strip", z.ZodTypeAny, {
    request_id: string;
    action: "health";
}, {
    request_id: string;
    action: "health";
}>, z.ZodObject<{
    request_id: z.ZodString;
} & {
    action: z.ZodLiteral<"shutdown">;
}, "strip", z.ZodTypeAny, {
    request_id: string;
    action: "shutdown";
}, {
    request_id: string;
    action: "shutdown";
}>, z.ZodObject<{
    request_id: z.ZodString;
} & {
    action: z.ZodLiteral<"list_program_files">;
}, "strip", z.ZodTypeAny, {
    request_id: string;
    action: "list_program_files";
}, {
    request_id: string;
    action: "list_program_files";
}>, z.ZodObject<{
    request_id: z.ZodString;
} & {
    action: z.ZodLiteral<"reach_graph">;
}, "strip", z.ZodTypeAny, {
    request_id: string;
    action: "reach_graph";
}, {
    request_id: string;
    action: "reach_graph";
}>, z.ZodObject<{
    request_id: z.ZodString;
} & {
    action: z.ZodLiteral<"reach">;
    graph: z.ZodObject<{
        version: z.ZodLiteral<1>;
        strings: z.ZodArray<z.ZodString, "many">;
        files: z.ZodArray<z.ZodObject<{
            p: z.ZodNumber;
            g: z.ZodUnion<[z.ZodLiteral<0>, z.ZodLiteral<1>]>;
            d: z.ZodArray<z.ZodNumber, "many">;
            r: z.ZodArray<z.ZodArray<z.ZodNumber, "many">, "many">;
            l: z.ZodArray<z.ZodArray<z.ZodNumber, "many">, "many">;
            e: z.ZodArray<z.ZodNumber, "many">;
            t: z.ZodArray<z.ZodArray<z.ZodNumber, "many">, "many">;
            b: z.ZodArray<z.ZodTuple<[z.ZodNumber, z.ZodNumber, z.ZodArray<z.ZodNumber, "many">], null>, "many">;
            w: z.ZodArray<z.ZodNumber, "many">;
            s: z.ZodArray<z.ZodTuple<[z.ZodNumber, z.ZodArray<z.ZodNumber, "many">, z.ZodArray<z.ZodNumber, "many">], null>, "many">;
        }, "strip", z.ZodTypeAny, {
            b: [number, number, number[]][];
            s: [number, number[], number[]][];
            p: number;
            g: 0 | 1;
            d: number[];
            r: number[][];
            l: number[][];
            e: number[];
            t: number[][];
            w: number[];
        }, {
            b: [number, number, number[]][];
            s: [number, number[], number[]][];
            p: number;
            g: 0 | 1;
            d: number[];
            r: number[][];
            l: number[][];
            e: number[];
            t: number[][];
            w: number[];
        }>, "many">;
    }, "strip", z.ZodTypeAny, {
        version: 1;
        files: {
            b: [number, number, number[]][];
            s: [number, number[], number[]][];
            p: number;
            g: 0 | 1;
            d: number[];
            r: number[][];
            l: number[][];
            e: number[];
            t: number[][];
            w: number[];
        }[];
        strings: string[];
    }, {
        version: 1;
        files: {
            b: [number, number, number[]][];
            s: [number, number[], number[]][];
            p: number;
            g: 0 | 1;
            d: number[];
            r: number[][];
            l: number[][];
            e: number[];
            t: number[][];
            w: number[];
        }[];
        strings: string[];
    }>;
    changes: z.ZodArray<z.ZodObject<{
        file: z.ZodString;
        before: z.ZodString;
        after: z.ZodString;
    }, "strip", z.ZodTypeAny, {
        after: string;
        before: string;
        file: string;
    }, {
        after: string;
        before: string;
        file: string;
    }>, "many">;
}, "strip", z.ZodTypeAny, {
    request_id: string;
    action: "reach";
    graph: {
        version: 1;
        files: {
            b: [number, number, number[]][];
            s: [number, number[], number[]][];
            p: number;
            g: 0 | 1;
            d: number[];
            r: number[][];
            l: number[][];
            e: number[];
            t: number[][];
            w: number[];
        }[];
        strings: string[];
    };
    changes: {
        after: string;
        before: string;
        file: string;
    }[];
}, {
    request_id: string;
    action: "reach";
    graph: {
        version: 1;
        files: {
            b: [number, number, number[]][];
            s: [number, number[], number[]][];
            p: number;
            g: 0 | 1;
            d: number[];
            r: number[][];
            l: number[][];
            e: number[];
            t: number[][];
            w: number[];
        }[];
        strings: string[];
    };
    changes: {
        after: string;
        before: string;
        file: string;
    }[];
}>, z.ZodObject<{
    request_id: z.ZodString;
} & {
    action: z.ZodLiteral<"add_program_files">;
    files: z.ZodArray<z.ZodString, "many">;
}, "strip", z.ZodTypeAny, {
    files: string[];
    request_id: string;
    action: "add_program_files";
}, {
    files: string[];
    request_id: string;
    action: "add_program_files";
}>, z.ZodObject<{
    request_id: z.ZodString;
} & {
    action: z.ZodLiteral<"position_rows">;
    files: z.ZodArray<z.ZodString, "many">;
    scan_root: z.ZodString;
    out_path: z.ZodString;
    service: z.ZodString;
}, "strip", z.ZodTypeAny, {
    service: string;
    files: string[];
    request_id: string;
    action: "position_rows";
    scan_root: string;
    out_path: string;
}, {
    service: string;
    files: string[];
    request_id: string;
    action: "position_rows";
    scan_root: string;
    out_path: string;
}>, z.ZodObject<{
    request_id: z.ZodString;
} & {
    action: z.ZodLiteral<"implementations">;
    queries: z.ZodArray<z.ZodObject<{
        file: z.ZodString;
        start: z.ZodNumber;
        end: z.ZodNumber;
    }, "strip", z.ZodTypeAny, {
        file: string;
        end: number;
        start: number;
    }, {
        file: string;
        end: number;
        start: number;
    }>, "many">;
    scan_root: z.ZodString;
}, "strip", z.ZodTypeAny, {
    queries: {
        file: string;
        end: number;
        start: number;
    }[];
    request_id: string;
    action: "implementations";
    scan_root: string;
}, {
    queries: {
        file: string;
        end: number;
        start: number;
    }[];
    request_id: string;
    action: "implementations";
    scan_root: string;
}>]>;
/**
 * Result type for parseRequest
 */
export type ParseResult = {
    success: true;
    request: SidecarRequest;
} | {
    success: false;
    error: string;
};
/**
 * Validates and parses incoming JSON into a typed SidecarRequest
 *
 * @param json - The raw JSON value to parse
 * @returns ParseResult with either the typed request or an error message
 */
export declare function parseRequest(json: unknown): ParseResult;
/**
 * Validates and parses incoming JSON, throwing on error
 *
 * @param json - The raw JSON value to parse
 * @returns The typed SidecarRequest
 * @throws Error if validation fails
 */
export declare function parseRequestOrThrow(json: unknown): SidecarRequest;
export type ValidatedInitRequest = z.infer<typeof InitRequestSchema>;
export type ValidatedBundleRequest = z.infer<typeof BundleRequestSchema>;
export type ValidatedInferRequest = z.infer<typeof InferRequestSchema>;
export type ValidatedHealthRequest = z.infer<typeof HealthRequestSchema>;
export type ValidatedShutdownRequest = z.infer<typeof ShutdownRequestSchema>;
export type ValidatedSidecarRequest = z.infer<typeof SidecarRequestSchema>;
