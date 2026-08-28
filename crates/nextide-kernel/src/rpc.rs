use std::fmt;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RpcRequest {
    pub method: String,
    pub payload: String,
}

impl RpcRequest {
    pub fn new(method: impl Into<String>, payload: impl Into<String>) -> Self {
        Self {
            method: method.into(),
            payload: payload.into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RpcResponse {
    pub payload: String,
}

#[derive(Debug, PartialEq, Eq)]
pub enum RpcError {
    EmptyMethod,
    MethodNotFound(String),
}

impl fmt::Display for RpcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyMethod => write!(f, "rpc method cannot be empty"),
            Self::MethodNotFound(method) => write!(f, "rpc method not found: {method}"),
        }
    }
}

impl std::error::Error for RpcError {}

pub trait RpcHandler {
    fn call(&mut self, request: RpcRequest) -> Result<RpcResponse, RpcError>;
}

#[derive(Default)]
pub struct LocalRpcRouter {
    handlers: std::collections::BTreeMap<String, Box<dyn FnMut(String) -> String + Send>>,
}

impl LocalRpcRouter {
    pub fn register<F>(&mut self, method: impl Into<String>, handler: F) -> Result<(), RpcError>
    where
        F: FnMut(String) -> String + Send + 'static,
    {
        let method = method.into();
        if method.trim().is_empty() {
            return Err(RpcError::EmptyMethod);
        }
        self.handlers.insert(method, Box::new(handler));
        Ok(())
    }
}

impl RpcHandler for LocalRpcRouter {
    fn call(&mut self, request: RpcRequest) -> Result<RpcResponse, RpcError> {
        let handler = self
            .handlers
            .get_mut(&request.method)
            .ok_or_else(|| RpcError::MethodNotFound(request.method.clone()))?;
        Ok(RpcResponse {
            payload: handler(request.payload),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_typed_request_to_handler() {
        let mut router = LocalRpcRouter::default();
        router
            .register("workspace.read", |payload| format!("read:{payload}"))
            .unwrap();
        let response = router
            .call(RpcRequest::new("workspace.read", "src/main.rs"))
            .unwrap();
        assert_eq!(response.payload, "read:src/main.rs");
    }

    #[test]
    fn reports_unknown_method() {
        let mut router = LocalRpcRouter::default();
        assert_eq!(
            router.call(RpcRequest::new("missing", "")),
            Err(RpcError::MethodNotFound("missing".into()))
        );
    }
}
