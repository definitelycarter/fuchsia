use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum HttpError {
    #[error("host '{host}' is not in allowed_hosts")]
    HostNotAllowed { host: String },

    #[error("request failed: {0}")]
    RequestFailed(String),

    #[error("invalid url: {0}")]
    InvalidUrl(String),
}

#[derive(Debug, Clone)]
pub struct HttpRequest {
    pub method: String,
    pub url: String,
    pub headers: HashMap<String, String>,
    pub body: Option<String>,
}

#[derive(Debug, Clone)]
pub struct HttpResponse {
    pub status: u16,
    pub headers: HashMap<String, String>,
    pub body: String,
}

/// Policy controlling which hosts a component may reach.
#[derive(Debug, Clone, Default)]
pub struct HttpPolicy {
    /// Allowed host patterns. Supports wildcard prefix: `*.example.com`.
    /// An empty list means no outbound HTTP is permitted.
    pub allowed_hosts: Vec<String>,
}

impl HttpPolicy {
    pub fn allow_all() -> Self {
        Self {
            allowed_hosts: vec!["*".to_string()],
        }
    }

    fn is_host_allowed(&self, host: &str) -> bool {
        self.allowed_hosts.iter().any(|pattern| {
            if pattern == "*" {
                true
            } else if let Some(suffix) = pattern.strip_prefix("*.") {
                host == suffix || host.ends_with(&format!(".{suffix}"))
            } else {
                host == pattern
            }
        })
    }
}

/// HTTP client capability for task execution.
///
/// Validates each request against the policy before making the call.
/// Requests to disallowed hosts are rejected with `HttpError::HostNotAllowed`.
pub trait HttpHost: Send + Sync {
    fn request(
        &self,
        req: HttpRequest,
    ) -> Pin<Box<dyn Future<Output = Result<HttpResponse, HttpError>> + Send + '_>>;
}

/// HTTP host that rejects all requests.
///
/// Useful for tests and components that don't need HTTP.
pub struct NoopHttpHost;

impl HttpHost for NoopHttpHost {
    fn request(
        &self,
        _req: HttpRequest,
    ) -> Pin<Box<dyn Future<Output = Result<HttpResponse, HttpError>> + Send + '_>> {
        Box::pin(async move {
            Err(HttpError::RequestFailed(
                "http disabled (NoopHttpHost)".into(),
            ))
        })
    }
}

/// reqwest-backed HTTP host with policy enforcement.
pub struct ReqwestHttpHost {
    policy: HttpPolicy,
    client: reqwest::Client,
}

impl ReqwestHttpHost {
    pub fn new(policy: HttpPolicy) -> Self {
        Self {
            policy,
            client: reqwest::Client::new(),
        }
    }
}

impl HttpHost for ReqwestHttpHost {
    fn request(
        &self,
        req: HttpRequest,
    ) -> Pin<Box<dyn Future<Output = Result<HttpResponse, HttpError>> + Send + '_>> {
        Box::pin(async move {
            let url: reqwest::Url = req
                .url
                .parse()
                .map_err(|e: url::ParseError| HttpError::InvalidUrl(e.to_string()))?;

            let host = url
                .host_str()
                .ok_or_else(|| HttpError::InvalidUrl("missing host".to_string()))?;

            if !self.policy.is_host_allowed(host) {
                return Err(HttpError::HostNotAllowed {
                    host: host.to_string(),
                });
            }

            let method: reqwest::Method = req
                .method
                .parse()
                .map_err(|_| HttpError::RequestFailed(format!("invalid method: {}", req.method)))?;

            let mut builder = self.client.request(method, url);

            for (k, v) in &req.headers {
                builder = builder.header(k, v);
            }

            if let Some(body) = req.body {
                builder = builder.body(body);
            }

            let response = builder
                .send()
                .await
                .map_err(|e| HttpError::RequestFailed(e.to_string()))?;

            let status = response.status().as_u16();
            let headers = response
                .headers()
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
                .collect();
            let body = response
                .text()
                .await
                .map_err(|e| HttpError::RequestFailed(e.to_string()))?;

            Ok(HttpResponse {
                status,
                headers,
                body,
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_policy_exact_match() {
        let policy = HttpPolicy {
            allowed_hosts: vec!["api.example.com".to_string()],
        };
        assert!(policy.is_host_allowed("api.example.com"));
        assert!(!policy.is_host_allowed("evil.com"));
    }

    #[test]
    fn test_policy_wildcard() {
        let policy = HttpPolicy {
            allowed_hosts: vec!["*.googleapis.com".to_string()],
        };
        assert!(policy.is_host_allowed("storage.googleapis.com"));
        assert!(policy.is_host_allowed("googleapis.com"));
        assert!(!policy.is_host_allowed("evil.com"));
    }

    #[test]
    fn test_policy_allow_all() {
        let policy = HttpPolicy::allow_all();
        assert!(policy.is_host_allowed("anything.com"));
    }

    #[test]
    fn test_policy_empty_denies_all() {
        let policy = HttpPolicy::default();
        assert!(!policy.is_host_allowed("anything.com"));
    }
}
