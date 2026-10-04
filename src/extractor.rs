use crate::visitor::Json;
use std::collections::BTreeMap;
use swc_ecma_ast::*;

pub trait CoreExtractor {
    fn extract_json_fields_from_call(&self, expr_stmt: &ExprStmt) -> Option<Json> {
        if let Expr::Call(call) = &*expr_stmt.expr {
            self.extract_res_json_fields(call)
        } else {
            None
        }
    }

    fn extract_json_fields_from_return(&self, expr: &Expr) -> Option<Json> {
        if let Expr::Call(call) = expr {
            self.extract_res_json_fields(call)
        } else {
            None
        }
    }

    fn extract_fields_from_arrow(&self, arrow: &ArrowExpr) -> Json {
        match &*arrow.body {
            // For arrow functions with block bodies: (req, res) => { ... }
            BlockStmtOrExpr::BlockStmt(block) => {
                for stmt in &block.stmts {
                    if let Stmt::Expr(expr_stmt) = stmt
                        && let Some(json) = self.extract_json_fields_from_call(expr_stmt)
                    {
                        return json;
                    }

                    // Look for return statements
                    if let Stmt::Return(ret) = stmt
                        && let Some(expr) = &ret.arg
                        && let Some(json) = self.extract_json_fields_from_return(expr)
                    {
                        return json;
                    }
                }
            }
            // For arrow functions with expression bodies: (req, res) => res.json(...)
            BlockStmtOrExpr::Expr(expr) => {
                if let Some(json) = self.extract_json_fields_from_return(expr) {
                    return json;
                }
            }
        }

        // Default if we couldn't find any response
        Json::Null
    }

    // Extract response fields from a function declaration
    fn extract_fields_from_function_decl(&self, fn_decl: &FnDecl) -> Json {
        // Check if the function has a body
        if let Some(body) = &fn_decl.function.body {
            // Analyze each statement in the function body
            for stmt in &body.stmts {
                match stmt {
                    // For expressions like res.json({...})
                    Stmt::Expr(expr_stmt) => {
                        if let Some(json) = self.extract_json_fields_from_call(expr_stmt) {
                            return json;
                        }
                    }
                    // For return statements like return res.json({...})
                    Stmt::Return(return_stmt) => {
                        if let Some(expr) = &return_stmt.arg
                            && let Some(json) = self.extract_json_fields_from_return(expr)
                        {
                            return json;
                        }
                    }
                    // Handle nested blocks like if/else statements
                    Stmt::Block(block) => {
                        for nested_stmt in &block.stmts {
                            if let Stmt::Expr(expr_stmt) = nested_stmt
                                && let Some(json) = self.extract_json_fields_from_call(expr_stmt)
                            {
                                return json;
                            }
                        }
                    }
                    // Other statement types could be handled here if needed
                    _ => {}
                }
            }
        }

        // Default if we couldn't find any response
        Json::Null
    }

    // Extract response fields from a function expression
    fn extract_fields_from_function_expr(&self, fn_expr: &FnExpr) -> Json {
        // Check if the function has a body
        if let Some(body) = &fn_expr.function.body {
            // Analyze each statement in the function body
            for stmt in &body.stmts {
                match stmt {
                    // For expressions like res.json({...})
                    Stmt::Expr(expr_stmt) => {
                        if let Some(json) = self.extract_json_fields_from_call(expr_stmt) {
                            return json;
                        }
                    }

                    // For return statements like return res.json({...})
                    Stmt::Return(return_stmt) => {
                        if let Some(expr) = &return_stmt.arg
                            && let Some(json) = self.extract_json_fields_from_return(expr)
                        {
                            return json;
                        }
                    }

                    // Handle nested blocks like if/else statements
                    Stmt::Block(block) => {
                        for nested_stmt in &block.stmts {
                            if let Stmt::Expr(expr_stmt) = nested_stmt
                                && let Some(json) = self.extract_json_fields_from_call(expr_stmt)
                            {
                                return json;
                            }
                        }
                    }

                    // Other statement types could be handled here if needed
                    _ => {}
                }
            }
        }

        // Default if we couldn't find any response
        Json::Null
    }

    // Extract JSON structure from res.json(...)
    fn extract_res_json_fields(&self, call: &CallExpr) -> Option<Json> {
        let callee = call.callee.as_expr()?;
        let member = callee.as_member()?;

        // The method being invoked must be a response-sending method. Previously this
        // matched any call whose receiver was named `res` OR `json`, which mis-fired on
        // `JSON.stringify(...)`, `json.parse(...)`, or a local `const json = ...`, and
        // captured non-body calls like `res.cookie(...)` as a response body.
        let MemberProp::Ident(method) = &member.prop else {
            return None;
        };
        if !matches!(method.sym.as_ref(), "json" | "send" | "jsonp") {
            return None;
        }

        // The receiver should be the handler's response object. Matching on the
        // conventional parameter names keeps this framework-agnostic while avoiding the
        // false positives above.
        let obj = member.obj.as_ident()?;
        if !matches!(obj.sym.as_ref(), "res" | "response" | "reply") {
            return None;
        }

        let arg = call.args.first()?;
        // Extract the JSON structure from the argument
        Some(self.expr_to_json(&arg.expr))
    }

    // Convert an expression to a Json value
    fn expr_to_json(&self, expr: &Expr) -> Json {
        match expr {
            // Handle literals
            Expr::Lit(lit) => match lit {
                Lit::Str(str_lit) => Json::String(str_lit.value.to_string()),
                Lit::Num(num) => Json::Number(num.value),
                Lit::Bool(b) => Json::Boolean(b.value),
                Lit::Null(_) => Json::Null,
                _ => Json::Null, // Other literals
            },

            // Handle arrays
            Expr::Array(arr) => {
                let values: Vec<Json> = arr
                    .elems
                    .iter()
                    .filter_map(|elem| elem.as_ref().map(|e| self.expr_to_json(&e.expr)))
                    .collect();
                Json::Array(values)
            }

            // Handle objects
            Expr::Object(obj) => {
                let mut map = BTreeMap::new();

                for prop in &obj.props {
                    if let PropOrSpread::Prop(boxed_prop) = prop
                        && let Prop::KeyValue(kv) = &**boxed_prop
                    {
                        // Extract key
                        let key = match &kv.key {
                            PropName::Ident(ident) => ident.sym.to_string(),
                            PropName::Str(str) => str.value.to_string(),
                            _ => continue, // Skip computed keys
                        };

                        // Extract value
                        let value = self.expr_to_json(&kv.value);

                        map.insert(key, value);
                    }
                }

                Json::Object(map)
            }

            // Other expressions (function calls, identifiers, etc.) - treat as null for now
            _ => Json::Null,
        }
    }

    fn extract_req_body_fields(&self, function_body: &BlockStmt) -> Option<Json> {
        // Examine each statement in the function body
        for stmt in &function_body.stmts {
            // Look for variable declarations that extract from req.body
            if let Stmt::Decl(Decl::Var(var_decl)) = stmt
                && let Some(json) = self.extract_req_body_from_var_decl(var_decl)
            {
                return Some(json);
            }

            // Look for direct req.body usage
            if let Stmt::Expr(expr_stmt) = stmt
                && let Some(json) = self.extract_req_body_from_expr(&expr_stmt.expr)
            {
                return Some(json);
            }

            // Check if statements for validation logic
            if let Stmt::If(if_stmt) = stmt
                && let Some(json) = self.extract_req_body_from_condition(&if_stmt.test)
            {
                return Some(json);
            }
        }

        None
    }

    // Handle destructuring patterns: const { field1, field2 } = req.body
    fn extract_req_body_from_var_decl(&self, var_decl: &VarDecl) -> Option<Json> {
        for decl in &var_decl.decls {
            // Check if initialization is from req.body
            if let Some(init) = &decl.init
                && let Expr::Member(member) = &**init
                && let Expr::Ident(obj) = &*member.obj
                && obj.sym == "req"
                && let MemberProp::Ident(prop) = &member.prop
                && prop.sym == "body"
            {
                // Found req.body assignment

                // Extract fields from destructuring pattern
                if let Pat::Object(obj_pat) = &decl.name {
                    let mut fields = BTreeMap::new();

                    for prop in &obj_pat.props {
                        if let ObjectPatProp::Assign(assign_prop) = prop {
                            let field_name = assign_prop.key.sym.to_string();
                            fields.insert(field_name, Json::Null); // We don't know types yet
                        } else if let ObjectPatProp::KeyValue(kv_prop) = prop
                            && let PropName::Ident(key) = &kv_prop.key
                        {
                            let field_name = key.sym.to_string();
                            fields.insert(field_name, Json::Null);
                        }
                    }

                    if !fields.is_empty() {
                        return Some(Json::Object(fields));
                    }
                }
            }
        }

        None
    }

    // Handle direct access: if(req.body.field)
    fn extract_req_body_from_expr(&self, expr: &Expr) -> Option<Json> {
        if let Expr::Member(member) = expr
            && let Expr::Member(inner_member) = &*member.obj
            && let Expr::Ident(obj) = &*inner_member.obj
            && obj.sym == "req"
            && let MemberProp::Ident(body_prop) = &inner_member.prop
            && body_prop.sym == "body"
        {
            // Found req.body.something
            if let MemberProp::Ident(field_prop) = &member.prop {
                let field_name = field_prop.sym.to_string();
                let mut fields = BTreeMap::new();
                fields.insert(field_name, Json::Null);
                return Some(Json::Object(fields));
            }
        }

        None
    }

    // Handle validation conditions: if(!req.body.field)
    fn extract_req_body_from_condition(&self, expr: &Expr) -> Option<Json> {
        match expr {
            // Handle negation: if(!req.body.field)
            Expr::Unary(unary) => {
                if unary.op == UnaryOp::Bang {
                    return self.extract_req_body_from_expr(&unary.arg);
                }
            }

            // Handle binary operations: if(req.body.field === undefined)
            Expr::Bin(bin) => {
                if let Some(left_json) = self.extract_req_body_from_expr(&bin.left) {
                    return Some(left_json);
                }
                return self.extract_req_body_from_expr(&bin.right);
            }

            // Direct field access
            _ => return self.extract_req_body_from_expr(expr),
        }

        None
    }
}
