use reqwest::{Client, Url};
use serde_json::Value;
use std::{fmt, net::IpAddr, time::Duration};

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug)]
pub struct Error(pub &'static str);
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}
impl std::error::Error for Error {}

pub struct Http {
    pub client: Client,
    pub base: Url,
}
impl Http {
    pub fn new_for_exercise(base: &str, allow_remote: bool) -> Result<Self> {
        let http = Self::new(base)?;
        if !allow_remote && !is_loopback(&http.base) {
            return Err(Error("remote exercise requires --allow-remote-exercise"));
        }
        Ok(http)
    }

    pub fn new(base: &str) -> Result<Self> {
        let base = Url::parse(base).map_err(|_| Error("invalid API base"))?;
        if !matches!(base.scheme(), "http" | "https")
            || !base.username().is_empty()
            || base.password().is_some()
            || base.query().is_some()
            || base.fragment().is_some()
            || base.path() != "/"
        {
            return Err(Error("invalid API base"));
        }
        let loopback = is_loopback(&base);
        if base.scheme() == "http" && !loopback {
            return Err(Error("remote API base requires HTTPS"));
        }
        let mut client = Client::builder()
            .timeout(Duration::from_secs(5))
            .redirect(reqwest::redirect::Policy::none());
        if loopback {
            client = client.no_proxy();
        }
        let client = client
            .build()
            .map_err(|_| Error("HTTP client unavailable"))?;
        Ok(Self { client, base })
    }
    pub fn url(&self, link: &str) -> Result<Url> {
        let url = self
            .base
            .join(link)
            .map_err(|_| Error("invalid API link"))?;
        if url.origin() != self.base.origin()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
            || !url.path().starts_with("/bridge/v1/")
        {
            return Err(Error("unsafe API link"));
        }
        Ok(url)
    }
    pub async fn request(
        &self,
        method: reqwest::Method,
        link: &str,
        token: &str,
        body: Option<&Value>,
    ) -> Result<(u16, Value)> {
        let url = self.url(link)?;
        let mut request = self.client.request(method, url).bearer_auth(token);
        if let Some(body) = body {
            let encoded = serde_json::to_vec(body).map_err(|_| Error("invalid request JSON"))?;
            if encoded.len() > 64 * 1024 {
                return Err(Error("request exceeds bound"));
            }
            request = request
                .header("content-type", "application/json")
                .body(encoded);
        }
        let response = request
            .send()
            .await
            .map_err(|_| Error("HTTP request failed"))?;
        let status = response.status().as_u16();
        let bytes = bounded(response, 1024 * 1024).await?;
        let body = serde_json::from_slice(&bytes).map_err(|_| Error("invalid JSON response"))?;
        Ok((status, body))
    }
}

fn is_loopback(base: &Url) -> bool {
    base.host_str().is_some_and(|host| {
        host == "localhost"
            || host
                .strip_prefix('[')
                .and_then(|host| host.strip_suffix(']'))
                .unwrap_or(host)
                .parse::<IpAddr>()
                .is_ok_and(|address| address.is_loopback())
    })
}

pub async fn bounded(mut response: reqwest::Response, limit: usize) -> Result<Vec<u8>> {
    if response
        .content_length()
        .is_some_and(|size| size > limit as u64)
    {
        return Err(Error("response exceeds bound"));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| Error("HTTP body failed"))?
    {
        if chunk.len() > limit.saturating_sub(bytes.len()) {
            return Err(Error("response exceeds bound"));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::Http;

    #[test]
    fn loopback_hosts_are_accepted_and_remote_plaintext_is_rejected() {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        for base in [
            "http://localhost:8080",
            "http://127.0.0.1:8080",
            "http://[::1]:8080",
            "https://ems.example:443",
            "https://localhost:8080",
            "https://127.0.0.1:8080",
            "https://[::1]:8080",
        ] {
            assert!(Http::new(base).is_ok(), "{base}");
        }
        for base in [
            "http://ems.example:8080",
            "http://192.0.2.1:8080",
            "http://localhost.example:8080",
            "http://localhost.:8080",
            "http://[2001:db8::1]:8080",
        ] {
            assert!(Http::new(base).is_err(), "{base}");
        }
    }
}
