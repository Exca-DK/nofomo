use std::path::Path;

use anyhow::{Context, Result, bail};
use reqwest::{Client, StatusCode, Url};
use serde::Deserialize;
use serde_json::{Value, json};

/// Loopback address and token a running daemon publishes beside its database.
#[derive(Clone, Debug, Deserialize)]
pub struct Endpoint {
    pub(crate) url: Url,
    pub(crate) token: String,
}

impl Endpoint {
    /// Reads the manifest a running daemon wrote.
    ///
    /// # Errors
    ///
    /// Returns an error if the file is missing or malformed, or if it names
    /// anything other than a loopback address carrying a token.
    pub fn read(path: &Path) -> Result<Self> {
        let endpoint: Self = serde_json::from_slice(
            &std::fs::read(path)
                .with_context(|| format!("cannot read admin manifest {}", path.display()))?,
        )
        .with_context(|| format!("invalid admin manifest {}", path.display()))?;
        if endpoint.url.scheme() != "http"
            || !endpoint
                .url
                .host_str()
                .is_some_and(|host| matches!(host, "127.0.0.1" | "::1"))
            || endpoint.token.is_empty()
        {
            bail!("admin manifest is not a valid loopback endpoint");
        }
        Ok(endpoint)
    }
}

/// Authors through the admin surface of the daemon that holds the database.
pub struct AdminClient {
    endpoint: Endpoint,
    http: Client,
}

impl AdminClient {
    /// Attaches to the daemon whose manifest sits at `manifest`.
    ///
    /// # Errors
    ///
    /// Returns an error if no usable manifest is there.
    pub fn attach(manifest: &Path) -> Result<Self> {
        Ok(Self {
            endpoint: Endpoint::read(manifest)?,
            http: Client::new(),
        })
    }

    /// Calls one admin tool and returns its result.
    ///
    /// # Errors
    ///
    /// Returns an error if the daemon cannot be reached, if it restarted with a
    /// new token, or if the tool refused the arguments.
    pub async fn call(&self, tool: &str, arguments: Value) -> Result<Value> {
        let response = self
            .http
            .post(self.endpoint.url.clone())
            .bearer_auth(&self.endpoint.token)
            // The daemon answers in plain JSON, but streamable HTTP still checks Accept.
            .header(
                reqwest::header::ACCEPT,
                "application/json, text/event-stream",
            )
            .json(&json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": { "name": tool, "arguments": arguments },
            }))
            .send()
            .await
            .with_context(|| format!("cannot reach the daemon to call {tool}"))?;
        if response.status() == StatusCode::UNAUTHORIZED {
            bail!("the daemon restarted since it wrote its manifest; run this again");
        }
        let body: Value = response
            .error_for_status()
            .with_context(|| format!("the daemon rejected the {tool} request"))?
            .json()
            .await
            .with_context(|| format!("the daemon sent an unreadable {tool} response"))?;
        if let Some(error) = body.get("error") {
            bail!("{tool}: {}", detail(error.get("message")));
        }
        let result = body
            .get("result")
            .with_context(|| format!("the {tool} response carries no result"))?;
        if result.get("isError").and_then(Value::as_bool) == Some(true) {
            bail!("{tool}: {}", detail(first_text(result)));
        }
        Ok(result.clone())
    }
}

fn first_text(result: &Value) -> Option<&Value> {
    result.get("content")?.as_array()?.first()?.get("text")
}

fn detail(value: Option<&Value>) -> String {
    value
        .and_then(Value::as_str)
        .unwrap_or("no detail given")
        .to_string()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use serde_json::json;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    use super::{AdminClient, Endpoint};

    #[tokio::test]
    async fn a_stored_level_comes_back_as_the_tools_result() {
        let (manifest, _server) = stub(
            "stored",
            200,
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": {
                    "content": [{ "type": "text", "text": "{\"id\":\"l-1\"}" }],
                    "structuredContent": { "id": "l-1" },
                    "isError": false
                }
            }),
        )
        .await;

        let result = AdminClient::attach(&manifest)
            .unwrap()
            .call("set_level", json!({ "id": "l-1" }))
            .await
            .unwrap();

        assert_eq!(result["structuredContent"]["id"], "l-1");
        clean(&manifest);
    }

    #[tokio::test]
    async fn a_tool_that_refuses_the_arguments_is_an_error_here_too() {
        let (manifest, _server) = stub(
            "refused",
            200,
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "error": { "code": -32602, "message": "strategy does not exist" }
            }),
        )
        .await;

        let error = AdminClient::attach(&manifest)
            .unwrap()
            .call("set_level", json!({}))
            .await
            .expect_err("a refusal must not read as success")
            .to_string();

        assert!(error.contains("strategy does not exist"), "{error}");
        clean(&manifest);
    }

    // A tool can also refuse inside a successful envelope, which is easy to miss.
    #[tokio::test]
    async fn an_error_flag_inside_the_result_is_not_success() {
        let (manifest, _server) = stub(
            "flagged",
            200,
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": {
                    "content": [{ "type": "text", "text": "slippage_bps must not exceed 500" }],
                    "isError": true
                }
            }),
        )
        .await;

        let error = AdminClient::attach(&manifest)
            .unwrap()
            .call("set_level", json!({}))
            .await
            .expect_err("isError must not read as success")
            .to_string();

        assert!(error.contains("must not exceed 500"), "{error}");
        clean(&manifest);
    }

    #[tokio::test]
    async fn a_token_the_daemon_no_longer_accepts_says_to_retry() {
        let (manifest, _server) = stub("stale", 401, json!({})).await;

        let error = AdminClient::attach(&manifest)
            .unwrap()
            .call("status", json!({}))
            .await
            .expect_err("401 must not read as success")
            .to_string();

        assert!(error.contains("restarted"), "{error}");
        clean(&manifest);
    }

    #[test]
    fn a_manifest_pointing_away_from_loopback_is_refused() {
        for (name, body) in [
            (
                "remote",
                json!({ "url": "http://example.invalid/", "token": "t" }),
            ),
            (
                "no-token",
                json!({ "url": "http://127.0.0.1:1/", "token": "" }),
            ),
        ] {
            let manifest = temp_path(name);
            std::fs::write(&manifest, body.to_string()).unwrap();
            assert!(
                Endpoint::read(&manifest).is_err(),
                "{name} must not be trusted"
            );
            clean(&manifest);
        }
    }

    #[test]
    fn a_missing_manifest_says_which_file_is_absent() {
        let manifest = temp_path("absent");
        let error = Endpoint::read(&manifest).unwrap_err().to_string();
        assert!(error.contains("cannot read admin manifest"), "{error}");
        assert!(error.contains("absent"), "{error}");
    }

    fn temp_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "tempo-agentic-admin-client-{}-{name}.json",
            std::process::id()
        ))
    }

    fn clean(path: &PathBuf) {
        let _ = std::fs::remove_file(path);
    }

    // Answers exactly one request, which is all any single call needs.
    async fn stub(
        name: &str,
        status: u16,
        body: serde_json::Value,
    ) -> (PathBuf, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = vec![0; 4096];
            let _ = stream.read(&mut request).await.unwrap();
            let body = serde_json::to_vec(&body).unwrap();
            let head = format!(
                "HTTP/1.1 {status} x\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                body.len()
            );
            stream.write_all(head.as_bytes()).await.unwrap();
            stream.write_all(&body).await.unwrap();
        });
        let manifest = temp_path(name);
        std::fs::write(
            &manifest,
            json!({ "url": format!("http://{address}/"), "token": "test-token" }).to_string(),
        )
        .unwrap();
        (manifest, server)
    }
}
