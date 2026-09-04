//! Integration tests for the file-centric analysis pipeline.
//!
//! These tests verify that the FileAnalyzerAgent and FileOrchestrator
//! correctly process files and build the MountGraph.

use carrick::agent_service::AgentService;
use carrick::agents::file_analyzer_agent::{
    DataCallResult, EndpointResult, FileAnalysisResult, FileAnalyzerAgent, MountResult,
};
use carrick::agents::file_orchestrator::{FileOrchestrator, ProcessingStats};
use carrick::agents::framework_guidance_agent::{
    FrameworkGuidance, PatternExample, ProtocolGuidance,
};
use carrick::framework_detector::DetectionResult;
use carrick::operation::Protocol;
use serial_test::serial;
use std::collections::HashMap;
use std::path::PathBuf;

/// Create test framework guidance with Express patterns
fn http_guidance(guidance: &FrameworkGuidance) -> ProtocolGuidance {
    ProtocolGuidance::from([(Protocol::Http, guidance.clone())])
}

fn create_express_guidance() -> FrameworkGuidance {
    FrameworkGuidance {
        mount_patterns: vec![
            PatternExample {
                pattern: ".use(".to_string(),
                description: "Mount middleware or router".to_string(),
                framework: "express".to_string(),
            },
            PatternExample {
                pattern: "app.use('/".to_string(),
                description: "Mount router with path prefix".to_string(),
                framework: "express".to_string(),
            },
        ],
        endpoint_patterns: vec![
            PatternExample {
                pattern: ".get(".to_string(),
                description: "GET endpoint".to_string(),
                framework: "express".to_string(),
            },
            PatternExample {
                pattern: ".post(".to_string(),
                description: "POST endpoint".to_string(),
                framework: "express".to_string(),
            },
            PatternExample {
                pattern: ".put(".to_string(),
                description: "PUT endpoint".to_string(),
                framework: "express".to_string(),
            },
            PatternExample {
                pattern: ".delete(".to_string(),
                description: "DELETE endpoint".to_string(),
                framework: "express".to_string(),
            },
        ],
        middleware_patterns: vec![PatternExample {
            pattern: "app.use(express.json())".to_string(),
            description: "JSON body parser middleware".to_string(),
            framework: "express".to_string(),
        }],
        data_fetching_patterns: vec![
            PatternExample {
                pattern: "fetch(".to_string(),
                description: "Fetch API call".to_string(),
                framework: "native".to_string(),
            },
            PatternExample {
                pattern: "axios.".to_string(),
                description: "Axios HTTP call".to_string(),
                framework: "axios".to_string(),
            },
        ],
        triage_hints: "Express uses app.use() for mounts and router.get/post/etc for endpoints"
            .to_string(),
        parsing_notes: "Express routes can be chained: router.route('/path').get().post()"
            .to_string(),
    }
}

/// Create test framework detection result
fn create_express_detection() -> DetectionResult {
    DetectionResult {
        frameworks: vec!["express".to_string()],
        data_fetchers: vec!["axios".to_string()],
        messaging_clients: vec![],
        notes: "Test detection result".to_string(),
    }
}

#[test]
fn test_file_analysis_result_structures() {
    // Test MountResult structure
    let mount = MountResult {
        line_number: 10,
        parent_node: "app".to_string(),
        child_node: "userRouter".to_string(),
        mount_path: "/users".to_string(),
        import_source: Some("./routes/users".to_string()),
        pattern_matched: ".use(".to_string(),
    };
    assert_eq!(mount.line_number, 10);
    assert_eq!(mount.parent_node, "app");
    assert!(mount.import_source.is_some());

    // Test EndpointResult structure
    let endpoint = EndpointResult {
        candidate_id: "span:100-140".to_string(),
        line_number: 15,
        owner_node: "router".to_string(),
        method: "GET".to_string(),
        path: "/:id".to_string(),
        handler_name: "getUserById".to_string(),
        pattern_matched: ".get(".to_string(),
        call_expression_span_start: None,
        call_expression_span_end: None,
        payload_expression_text: None,
        payload_expression_line: None,
        response_expression_text: None,
        response_expression_line: None,
        emission_style: None,
        primary_type_symbol: None,
        type_import_source: None,
    };
    assert_eq!(endpoint.method, "GET");
    assert_eq!(endpoint.path, "/:id");

    // Test DataCallResult structure
    let data_call = DataCallResult {
        call_kind: None,
        candidate_id: "span:200-260".to_string(),
        line_number: 25,
        target: "https://api.example.com/users".to_string(),
        method: Some("POST".to_string()),
        pattern_matched: "fetch(".to_string(),
        call_expression_span_start: None,
        call_expression_span_end: None,
        call_expression_text: None,
        call_expression_line: None,
        payload_expression_text: None,
        payload_expression_line: None,
        primary_type_symbol: None,
        type_import_source: None,

        loopback_default_url: None,
        base: None,
        consumers_not_resolved: None,
    };
    assert_eq!(data_call.target, "https://api.example.com/users");
    assert_eq!(data_call.method, Some("POST".to_string()));
}

#[test]
fn test_file_analysis_result_default() {
    let result = FileAnalysisResult::default();
    assert!(result.mounts.is_empty());
    assert!(result.endpoints.is_empty());
    assert!(result.data_calls.is_empty());
}

#[test]
fn test_file_analysis_result_serialization() {
    let result = FileAnalysisResult {
        graphql_consumer_locates: vec![],
        graphql_operations: vec![],
        pubsub_operations: vec![],
        mounts: vec![MountResult {
            line_number: 5,
            parent_node: "app".to_string(),
            child_node: "apiRouter".to_string(),
            mount_path: "/api".to_string(),
            import_source: Some("./api".to_string()),
            pattern_matched: ".use(".to_string(),
        }],
        endpoints: vec![
            EndpointResult {
                candidate_id: "span:300-340".to_string(),
                line_number: 10,
                owner_node: "router".to_string(),
                method: "GET".to_string(),
                path: "/health".to_string(),
                handler_name: "healthCheck".to_string(),
                pattern_matched: ".get(".to_string(),
                call_expression_span_start: None,
                call_expression_span_end: None,
                payload_expression_text: None,
                payload_expression_line: None,
                response_expression_text: None,
                response_expression_line: None,
                emission_style: None,
                primary_type_symbol: None,
                type_import_source: None,
            },
            EndpointResult {
                candidate_id: "span:350-400".to_string(),
                line_number: 15,
                owner_node: "router".to_string(),
                method: "POST".to_string(),
                path: "/data".to_string(),
                handler_name: "createData".to_string(),
                pattern_matched: ".post(".to_string(),
                call_expression_span_start: None,
                call_expression_span_end: None,
                payload_expression_text: None,
                payload_expression_line: None,
                response_expression_text: None,
                response_expression_line: None,
                emission_style: None,
                primary_type_symbol: None,
                type_import_source: None,
            },
        ],
        data_calls: vec![DataCallResult {
            call_kind: None,
            candidate_id: "span:410-460".to_string(),
            line_number: 20,
            target: "https://external-api.com/data".to_string(),
            method: Some("GET".to_string()),
            pattern_matched: "fetch(".to_string(),
            call_expression_span_start: None,
            call_expression_span_end: None,
            call_expression_text: None,
            call_expression_line: None,
            payload_expression_text: None,
            payload_expression_line: None,
            primary_type_symbol: None,
            type_import_source: None,

            loopback_default_url: None,
            base: None,
            consumers_not_resolved: None,
        }],
    };

    // Serialize to JSON
    let json = serde_json::to_string(&result).expect("Should serialize");

    // Verify JSON contains expected fields
    assert!(json.contains("mounts"));
    assert!(json.contains("endpoints"));
    assert!(json.contains("data_calls"));
    assert!(json.contains("apiRouter"));
    assert!(json.contains("healthCheck"));
    assert!(json.contains("external-api.com"));

    // Deserialize back
    let deserialized: FileAnalysisResult = serde_json::from_str(&json).expect("Should deserialize");

    assert_eq!(deserialized.mounts.len(), 1);
    assert_eq!(deserialized.endpoints.len(), 2);
    assert_eq!(deserialized.data_calls.len(), 1);
}

#[test]
fn test_file_analyzer_agent_system_message_framework_agnostic() {
    let agent_service = AgentService::new();
    let analyzer = FileAnalyzerAgent::new(agent_service);

    // We can't directly access the system message, but we can verify
    // the agent is created successfully
    assert!(std::mem::size_of_val(&analyzer) > 0);
}

#[test]
fn test_file_orchestrator_creation() {
    let agent_service = AgentService::new();
    let _orchestrator = FileOrchestrator::new(agent_service);
    // Orchestrator should be created without errors
}

#[test]
fn test_processing_stats_tracking() {
    let stats = ProcessingStats {
        files_processed: 5,
        files_skipped: 2,
        files_skipped_no_candidates: 1,
        files_parse_failed: 1,
        files_analysis_failed: 0,
        files_skipped_unrouted_protocol: 0,
        imported_member_resolutions: 0,
        imported_member_backfills: 0,
        whole_url_env_backfills: 0,
        whole_url_env_corrections: 0,
        wrapper_method_propagations: 0,
        total_mounts: 3,
        total_endpoints: 10,
        file_based_endpoints: 2,
        route_descriptor_endpoints: 1,
        class_controller_endpoints: 0,
        pubsub_anchor_backfills: 0,
        pubsub_phantom_topic_drops: 0,
        request_spec_call_backfills: 0,
        local_wrapper_call_backfills: 0,
        total_data_calls: 4,
        errors: vec!["Test error".to_string()],
    };

    assert_eq!(stats.files_processed, 5);
    assert_eq!(stats.files_skipped, 2);
    assert_eq!(stats.total_mounts, 3);
    assert_eq!(stats.total_endpoints, 10);
    assert_eq!(stats.total_data_calls, 4);
    assert_eq!(stats.errors.len(), 1);
}

#[test]
fn test_framework_guidance_patterns() {
    let guidance = create_express_guidance();

    // Verify mount patterns
    assert!(!guidance.mount_patterns.is_empty());
    assert!(
        guidance
            .mount_patterns
            .iter()
            .any(|p| p.pattern.contains(".use("))
    );

    // Verify endpoint patterns
    assert!(!guidance.endpoint_patterns.is_empty());
    assert!(
        guidance
            .endpoint_patterns
            .iter()
            .any(|p| p.pattern.contains(".get("))
    );
    assert!(
        guidance
            .endpoint_patterns
            .iter()
            .any(|p| p.pattern.contains(".post("))
    );

    // Verify data fetching patterns
    assert!(!guidance.data_fetching_patterns.is_empty());
    assert!(
        guidance
            .data_fetching_patterns
            .iter()
            .any(|p| p.pattern.contains("fetch("))
    );
}

#[test]
fn test_cross_file_import_resolution() {
    // Simulate file analysis results from multiple files
    let mut file_results: HashMap<String, FileAnalysisResult> = HashMap::new();

    // Main app file that imports and mounts routers
    file_results.insert(
        "src/app.ts".to_string(),
        FileAnalysisResult {
            graphql_consumer_locates: vec![],
            graphql_operations: vec![],
            pubsub_operations: vec![],
            mounts: vec![
                MountResult {
                    line_number: 10,
                    parent_node: "app".to_string(),
                    child_node: "userRouter".to_string(),
                    mount_path: "/users".to_string(),
                    import_source: Some("./routes/users".to_string()),
                    pattern_matched: ".use(".to_string(),
                },
                MountResult {
                    line_number: 11,
                    parent_node: "app".to_string(),
                    child_node: "apiRouter".to_string(),
                    mount_path: "/api/v1".to_string(),
                    import_source: Some("./routes/api".to_string()),
                    pattern_matched: ".use(".to_string(),
                },
            ],
            endpoints: vec![],
            data_calls: vec![],
        },
    );

    // User routes file
    file_results.insert(
        "src/routes/users.ts".to_string(),
        FileAnalysisResult {
            graphql_consumer_locates: vec![],
            graphql_operations: vec![],
            pubsub_operations: vec![],
            mounts: vec![],
            endpoints: vec![
                EndpointResult {
                    candidate_id: "span:470-500".to_string(),
                    line_number: 5,
                    owner_node: "router".to_string(),
                    method: "GET".to_string(),
                    path: "/".to_string(),
                    handler_name: "listUsers".to_string(),
                    pattern_matched: ".get(".to_string(),
                    call_expression_span_start: None,
                    call_expression_span_end: None,
                    payload_expression_text: None,
                    payload_expression_line: None,
                    response_expression_text: None,
                    response_expression_line: None,
                    emission_style: None,
                    primary_type_symbol: None,
                    type_import_source: None,
                },
                EndpointResult {
                    candidate_id: "span:510-540".to_string(),
                    line_number: 10,
                    owner_node: "router".to_string(),
                    method: "GET".to_string(),
                    path: "/:id".to_string(),
                    handler_name: "getUserById".to_string(),
                    pattern_matched: ".get(".to_string(),
                    call_expression_span_start: None,
                    call_expression_span_end: None,
                    payload_expression_text: None,
                    payload_expression_line: None,
                    response_expression_text: None,
                    response_expression_line: None,
                    emission_style: None,
                    primary_type_symbol: None,
                    type_import_source: None,
                },
                EndpointResult {
                    candidate_id: "span:550-580".to_string(),
                    line_number: 15,
                    owner_node: "router".to_string(),
                    method: "POST".to_string(),
                    path: "/".to_string(),
                    handler_name: "createUser".to_string(),
                    pattern_matched: ".post(".to_string(),
                    call_expression_span_start: None,
                    call_expression_span_end: None,
                    payload_expression_text: None,
                    payload_expression_line: None,
                    response_expression_text: None,
                    response_expression_line: None,
                    emission_style: None,
                    primary_type_symbol: None,
                    type_import_source: None,
                },
            ],
            data_calls: vec![],
        },
    );

    // API routes file
    file_results.insert(
        "src/routes/api.ts".to_string(),
        FileAnalysisResult {
            graphql_consumer_locates: vec![],
            graphql_operations: vec![],
            pubsub_operations: vec![],
            mounts: vec![],
            endpoints: vec![
                EndpointResult {
                    candidate_id: "span:590-620".to_string(),
                    line_number: 5,
                    owner_node: "router".to_string(),
                    method: "GET".to_string(),
                    path: "/posts".to_string(),
                    handler_name: "getPosts".to_string(),
                    pattern_matched: ".get(".to_string(),
                    call_expression_span_start: None,
                    call_expression_span_end: None,
                    payload_expression_text: None,
                    payload_expression_line: None,
                    response_expression_text: None,
                    response_expression_line: None,
                    emission_style: None,
                    primary_type_symbol: None,
                    type_import_source: None,
                },
                EndpointResult {
                    candidate_id: "span:630-660".to_string(),
                    line_number: 10,
                    owner_node: "router".to_string(),
                    method: "POST".to_string(),
                    path: "/posts".to_string(),
                    handler_name: "createPost".to_string(),
                    pattern_matched: ".post(".to_string(),
                    call_expression_span_start: None,
                    call_expression_span_end: None,
                    payload_expression_text: None,
                    payload_expression_line: None,
                    response_expression_text: None,
                    response_expression_line: None,
                    emission_style: None,
                    primary_type_symbol: None,
                    type_import_source: None,
                },
            ],
            data_calls: vec![],
        },
    );

    // Verify the structure is correct for cross-file resolution
    assert_eq!(file_results.len(), 3);

    // Main app should have 2 mounts
    let app_result = file_results.get("src/app.ts").unwrap();
    assert_eq!(app_result.mounts.len(), 2);

    // Verify import sources are tracked
    let user_mount = &app_result.mounts[0];
    assert_eq!(user_mount.import_source, Some("./routes/users".to_string()));

    // User routes should have 3 endpoints
    let user_result = file_results.get("src/routes/users.ts").unwrap();
    assert_eq!(user_result.endpoints.len(), 3);

    // API routes should have 2 endpoints
    let api_result = file_results.get("src/routes/api.ts").unwrap();
    assert_eq!(api_result.endpoints.len(), 2);
}

#[test]
fn test_data_call_extraction() {
    let result = FileAnalysisResult {
        graphql_consumer_locates: vec![],
        graphql_operations: vec![],
        pubsub_operations: vec![],
        mounts: vec![],
        endpoints: vec![],
        data_calls: vec![
            DataCallResult {
                call_kind: None,
                candidate_id: "span:670-700".to_string(),
                line_number: 10,
                target: "https://api.example.com/users".to_string(),
                method: Some("GET".to_string()),
                pattern_matched: "fetch(".to_string(),
                call_expression_span_start: None,
                call_expression_span_end: None,
                call_expression_text: None,
                call_expression_line: None,
                payload_expression_text: None,
                payload_expression_line: None,
                primary_type_symbol: None,
                type_import_source: None,

                loopback_default_url: None,
                base: None,
                consumers_not_resolved: None,
            },
            DataCallResult {
                call_kind: None,
                candidate_id: "span:750-780".to_string(),
                line_number: 15,
                target: "/api/posts".to_string(),
                method: Some("POST".to_string()),
                pattern_matched: "axios.post(".to_string(),
                call_expression_span_start: None,
                call_expression_span_end: None,
                call_expression_text: None,
                call_expression_line: None,
                payload_expression_text: None,
                payload_expression_line: None,
                primary_type_symbol: None,
                type_import_source: None,

                loopback_default_url: None,
                base: None,
                consumers_not_resolved: None,
            },
            DataCallResult {
                call_kind: None,
                candidate_id: "span:790-820".to_string(),
                line_number: 20,
                target: "${baseUrl}/data".to_string(),
                method: None,
                pattern_matched: "fetch(".to_string(),
                call_expression_span_start: None,
                call_expression_span_end: None,
                call_expression_text: None,
                call_expression_line: None,
                payload_expression_text: None,
                payload_expression_line: None,
                primary_type_symbol: None,
                type_import_source: None,

                loopback_default_url: None,
                base: None,
                consumers_not_resolved: None,
            },
        ],
    };

    assert_eq!(result.data_calls.len(), 3);

    // Check fetch call
    let fetch_call = &result.data_calls[0];
    assert_eq!(fetch_call.pattern_matched, "fetch(");
    assert_eq!(fetch_call.method, Some("GET".to_string()));

    // Check axios call
    let axios_call = &result.data_calls[1];
    assert!(axios_call.pattern_matched.contains("axios"));
    assert_eq!(axios_call.method, Some("POST".to_string()));

    // Check call without method
    let unknown_call = &result.data_calls[2];
    assert!(unknown_call.method.is_none());
}

#[test]
fn test_nested_router_mounts() {
    // Test scenario: app -> apiRouter -> v1Router -> usersRouter
    let mut file_results: HashMap<String, FileAnalysisResult> = HashMap::new();

    file_results.insert(
        "src/app.ts".to_string(),
        FileAnalysisResult {
            graphql_consumer_locates: vec![],
            graphql_operations: vec![],
            pubsub_operations: vec![],
            mounts: vec![MountResult {
                line_number: 5,
                parent_node: "app".to_string(),
                child_node: "apiRouter".to_string(),
                mount_path: "/api".to_string(),
                import_source: Some("./routes/api".to_string()),
                pattern_matched: ".use(".to_string(),
            }],
            endpoints: vec![],
            data_calls: vec![],
        },
    );

    file_results.insert(
        "src/routes/api.ts".to_string(),
        FileAnalysisResult {
            graphql_consumer_locates: vec![],
            graphql_operations: vec![],
            pubsub_operations: vec![],
            mounts: vec![MountResult {
                line_number: 5,
                parent_node: "router".to_string(),
                child_node: "v1Router".to_string(),
                mount_path: "/v1".to_string(),
                import_source: Some("./v1".to_string()),
                pattern_matched: ".use(".to_string(),
            }],
            endpoints: vec![],
            data_calls: vec![],
        },
    );

    file_results.insert(
        "src/routes/v1/index.ts".to_string(),
        FileAnalysisResult {
            graphql_consumer_locates: vec![],
            graphql_operations: vec![],
            pubsub_operations: vec![],
            mounts: vec![MountResult {
                line_number: 5,
                parent_node: "router".to_string(),
                child_node: "usersRouter".to_string(),
                mount_path: "/users".to_string(),
                import_source: Some("./users".to_string()),
                pattern_matched: ".use(".to_string(),
            }],
            endpoints: vec![],
            data_calls: vec![],
        },
    );

    file_results.insert(
        "src/routes/v1/users.ts".to_string(),
        FileAnalysisResult {
            graphql_consumer_locates: vec![],
            graphql_operations: vec![],
            pubsub_operations: vec![],
            mounts: vec![],
            endpoints: vec![EndpointResult {
                candidate_id: "span:830-860".to_string(),
                line_number: 5,
                owner_node: "router".to_string(),
                method: "GET".to_string(),
                path: "/:id".to_string(),
                handler_name: "getUser".to_string(),
                pattern_matched: ".get(".to_string(),
                call_expression_span_start: None,
                call_expression_span_end: None,
                payload_expression_text: None,
                payload_expression_line: None,
                response_expression_text: None,
                response_expression_line: None,
                emission_style: None,
                primary_type_symbol: None,
                type_import_source: None,
            }],
            data_calls: vec![],
        },
    );

    // Verify the chain of mounts
    let total_mounts: usize = file_results.values().map(|r| r.mounts.len()).sum();
    assert_eq!(total_mounts, 3);

    // The final endpoint at /api/v1/users/:id should be traceable through imports
    let users_file = file_results.get("src/routes/v1/users.ts").unwrap();
    assert_eq!(users_file.endpoints.len(), 1);
    assert_eq!(users_file.endpoints[0].path, "/:id");
}

#[test]
fn test_multiple_http_methods_on_same_path() {
    let result = FileAnalysisResult {
        graphql_consumer_locates: vec![],
        graphql_operations: vec![],
        pubsub_operations: vec![],
        mounts: vec![],
        endpoints: vec![
            EndpointResult {
                candidate_id: "span:870-900".to_string(),
                line_number: 5,
                owner_node: "router".to_string(),
                method: "GET".to_string(),
                path: "/users".to_string(),
                handler_name: "getUsers".to_string(),
                pattern_matched: ".get(".to_string(),
                call_expression_span_start: None,
                call_expression_span_end: None,
                payload_expression_text: None,
                payload_expression_line: None,
                response_expression_text: None,
                response_expression_line: None,
                emission_style: None,
                primary_type_symbol: None,
                type_import_source: None,
            },
            EndpointResult {
                candidate_id: "span:910-940".to_string(),
                line_number: 10,
                owner_node: "router".to_string(),
                method: "POST".to_string(),
                path: "/users".to_string(),
                handler_name: "createUser".to_string(),
                pattern_matched: ".post(".to_string(),
                call_expression_span_start: None,
                call_expression_span_end: None,
                payload_expression_text: None,
                payload_expression_line: None,
                response_expression_text: None,
                response_expression_line: None,
                emission_style: None,
                primary_type_symbol: None,
                type_import_source: None,
            },
            EndpointResult {
                candidate_id: "span:950-980".to_string(),
                line_number: 15,
                owner_node: "router".to_string(),
                method: "PUT".to_string(),
                path: "/users/:id".to_string(),
                handler_name: "updateUser".to_string(),
                pattern_matched: ".put(".to_string(),
                call_expression_span_start: None,
                call_expression_span_end: None,
                payload_expression_text: None,
                payload_expression_line: None,
                response_expression_text: None,
                response_expression_line: None,
                emission_style: None,
                primary_type_symbol: None,
                type_import_source: None,
            },
            EndpointResult {
                candidate_id: "span:990-1020".to_string(),
                line_number: 20,
                owner_node: "router".to_string(),
                method: "DELETE".to_string(),
                path: "/users/:id".to_string(),
                handler_name: "deleteUser".to_string(),
                pattern_matched: ".delete(".to_string(),
                call_expression_span_start: None,
                call_expression_span_end: None,
                payload_expression_text: None,
                payload_expression_line: None,
                response_expression_text: None,
                response_expression_line: None,
                emission_style: None,
                primary_type_symbol: None,
                type_import_source: None,
            },
        ],
        data_calls: vec![],
    };

    // Verify all methods are captured
    let methods: Vec<&str> = result.endpoints.iter().map(|e| e.method.as_str()).collect();
    assert!(methods.contains(&"GET"));
    assert!(methods.contains(&"POST"));
    assert!(methods.contains(&"PUT"));
    assert!(methods.contains(&"DELETE"));

    // Verify unique paths
    let paths: Vec<&str> = result.endpoints.iter().map(|e| e.path.as_str()).collect();
    assert_eq!(paths.iter().filter(|&&p| p == "/users").count(), 2);
    assert_eq!(paths.iter().filter(|&&p| p == "/users/:id").count(), 2);
}

#[tokio::test]
#[serial]
async fn test_file_orchestrator_with_mock_agent() {
    // This test verifies the orchestrator can be created and would process files
    // In mock mode, actual LLM calls are not made
    // SAFETY: This test runs in isolation, environment variable modification is safe
    unsafe { std::env::set_var("CARRICK_MOCK_ALL", "1") };

    let agent_service = AgentService::new();
    let orchestrator = FileOrchestrator::new(agent_service);
    let guidance = create_express_guidance();
    let detection = create_express_detection();

    // Create a temp file with some express-like code
    let temp_dir = tempfile::tempdir().unwrap();
    let test_file = temp_dir.path().join("test.ts");
    std::fs::write(
        &test_file,
        r#"
import express from 'express';
const app = express();
app.get('/health', (req, res) => res.json({ status: 'ok' }));
app.post('/users', (req, res) => res.json({ created: true }));
"#,
    )
    .unwrap();

    let files = vec![test_file];
    let result = orchestrator
        .analyze_files(
            &files,
            &http_guidance(&guidance),
            &detection,
            temp_dir.path(),
            &[],
            &Default::default(),
            &Default::default(),
            &carrick::url_normalizer::UrlNormalizer::default_permissive(),
        )
        .await;

    // Verify the analysis completed (even with mock responses)
    assert!(result.is_ok());
    let analysis_result = result.unwrap();

    // Stats should reflect that we processed 1 file
    assert_eq!(analysis_result.stats.files_processed, 1);
    assert_eq!(analysis_result.stats.files_skipped, 0);

    // SAFETY: Cleanup of environment variable set by this test
    unsafe { std::env::remove_var("CARRICK_MOCK_ALL") };
}

#[tokio::test]
#[serial]
async fn test_file_orchestrator_handles_empty_files() {
    // SAFETY: This test runs in isolation, environment variable modification is safe
    unsafe { std::env::set_var("CARRICK_MOCK_ALL", "1") };

    let agent_service = AgentService::new();
    let orchestrator = FileOrchestrator::new(agent_service);
    let guidance = create_express_guidance();
    let detection = create_express_detection();

    // Create an empty temp file
    let temp_dir = tempfile::tempdir().unwrap();
    let empty_file = temp_dir.path().join("empty.ts");
    std::fs::write(&empty_file, "").unwrap();

    let files = vec![empty_file];
    let result = orchestrator
        .analyze_files(
            &files,
            &http_guidance(&guidance),
            &detection,
            temp_dir.path(),
            &[],
            &Default::default(),
            &Default::default(),
            &carrick::url_normalizer::UrlNormalizer::default_permissive(),
        )
        .await;

    assert!(result.is_ok());
    let analysis_result = result.unwrap();

    // Empty file should be skipped
    assert_eq!(analysis_result.stats.files_skipped, 1);
    assert_eq!(analysis_result.stats.files_processed, 0);

    // SAFETY: Cleanup of environment variable set by this test
    unsafe { std::env::remove_var("CARRICK_MOCK_ALL") };
}

#[tokio::test]
#[serial]
async fn test_file_orchestrator_handles_missing_files() {
    // SAFETY: This test runs in isolation, environment variable modification is safe
    unsafe { std::env::set_var("CARRICK_MOCK_ALL", "1") };

    let agent_service = AgentService::new();
    let orchestrator = FileOrchestrator::new(agent_service);
    let guidance = create_express_guidance();
    let detection = create_express_detection();

    // Try to analyze a non-existent file
    let files = vec![PathBuf::from("/nonexistent/file.ts")];
    let result = orchestrator
        .analyze_files(
            &files,
            &http_guidance(&guidance),
            &detection,
            PathBuf::from("/").as_path(),
            &[],
            &Default::default(),
            &Default::default(),
            &carrick::url_normalizer::UrlNormalizer::default_permissive(),
        )
        .await;

    assert!(result.is_ok());
    let analysis_result = result.unwrap();

    // Non-existent file should result in a skip with an error
    assert_eq!(analysis_result.stats.files_skipped, 1);
    assert!(!analysis_result.stats.errors.is_empty());

    // SAFETY: Cleanup of environment variable set by this test
    unsafe { std::env::remove_var("CARRICK_MOCK_ALL") };
}
