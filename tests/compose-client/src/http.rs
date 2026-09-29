use reqwest::{Client, Method, Url};
use serde_json::{Value, json};
use std::time::Duration;

use crate::{Error, Result, config::Args, evidence};

struct Http {
    client: Client,
    base: Url,
    reader: String,
    controller: String,
}

impl Http {
    fn new(args: &Args) -> Result<Self> {
        let base = Url::parse(
            args.http_base
                .as_deref()
                .ok_or(Error("HTTP endpoint missing"))?,
        )
        .map_err(|_| Error("invalid HTTP endpoint"))?;
        if base.scheme() != "http"
            || !matches!(base.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"))
            || base.path() != "/"
            || !base.username().is_empty()
            || base.password().is_some()
            || base.query().is_some()
            || base.fragment().is_some()
        {
            return Err(Error(
                "HTTP exercise requires credential-free loopback endpoint",
            ));
        }
        let client = Client::builder()
            .timeout(Duration::from_secs(5))
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| Error("HTTP client unavailable"))?;
        Ok(Self {
            client,
            base,
            reader: args.read_token.clone().ok_or(Error("read token missing"))?,
            controller: args
                .control_token
                .clone()
                .ok_or(Error("control token missing"))?,
        })
    }

    async fn request(
        &self,
        method: Method,
        path: &str,
        control: bool,
        payload: Option<&Value>,
    ) -> Result<(u16, Value)> {
        let url = self
            .base
            .join(path)
            .map_err(|_| Error("invalid HTTP path"))?;
        if url.origin() != self.base.origin()
            || !url.path().starts_with("/bridge/v1/")
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
        {
            return Err(Error("unsafe HTTP response link"));
        }
        let mut call = self.client.request(method, url).bearer_auth(if control {
            &self.controller
        } else {
            &self.reader
        });
        if let Some(payload) = payload {
            let bytes =
                serde_json::to_vec(payload).map_err(|_| Error("HTTP command encoding failed"))?;
            if bytes.len() > 64 * 1024 {
                return Err(Error("HTTP command exceeds bound"));
            }
            call = call.header("content-type", "application/json").body(bytes);
        }
        let mut response = call
            .send()
            .await
            .map_err(|_| Error("HTTP request failed"))?;
        if response
            .content_length()
            .is_some_and(|size| size > 1024 * 1024)
        {
            return Err(Error("HTTP response exceeds bound"));
        }
        let status = response.status().as_u16();
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| Error("HTTP body failed"))?
        {
            if chunk.len() > (1024 * 1024_usize).saturating_sub(bytes.len()) {
                return Err(Error("HTTP response exceeds bound"));
            }
            bytes.extend_from_slice(&chunk);
        }
        let value =
            serde_json::from_slice(&bytes).map_err(|_| Error("HTTP response is not JSON"))?;
        Ok((status, value))
    }

    async fn snapshot(&self, id: &str, bridge_id: &str) -> Result<Option<Value>> {
        let path = station_url(&self.base, id, bridge_id)?;
        let (status, snapshot) = self
            .request(Method::GET, path.as_str(), false, None)
            .await?;
        if status == 404 {
            return Ok(None);
        }
        if status != 200 {
            return Err(Error("canonical station read refused"));
        }
        Ok(Some(snapshot))
    }

    async fn accepted(
        &self,
        status_url: &str,
        args: &Args,
        station: &crate::config::Station,
        resource: &Value,
        request_id: &str,
    ) -> Result<()> {
        for _ in 0..150 {
            let (status, result) = self.request(Method::GET, status_url, true, None).await?;
            if status != 200 {
                return Err(Error("command result query refused"));
            }
            if evidence::accepted(&result, &args.config, station, resource, request_id, None)? {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        Err(Error("native command response not observed"))
    }
}

pub async fn run(args: &Args) -> Result<()> {
    let http = Http::new(args)?;
    for station in &args.config.station {
        let snapshot = loop {
            if let Some(state) = http.snapshot(&station.id, &args.config.bridge_id).await?
                && evidence::resource(&state, &args.config, station).is_ok()
            {
                break state;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        };
        let resource = evidence::resource(&snapshot, &args.config, station)?;
        let old_ids = evidence::baseline(&snapshot)?;
        let started = command(
            &http,
            args,
            station,
            &resource,
            &station.authorization_reference,
            &old_ids,
            None,
        )
        .await?;
        command(
            &http,
            args,
            station,
            &resource,
            &started,
            &old_ids,
            Some(&started),
        )
        .await?;
        println!(
            "charging verified: protocol={} start=native+state stop=native+state",
            station.protocol
        );
    }
    Ok(())
}

async fn command(
    http: &Http,
    args: &Args,
    station: &crate::config::Station,
    resource: &Value,
    parameter: &str,
    old_ids: &[String],
    started: Option<&str>,
) -> Result<String> {
    let kind = if started.is_some() { "stop" } else { "start" };
    let request_id = format!("compose-{}-{kind}", uuid::Uuid::new_v4());
    let expires = (time::OffsetDateTime::now_utc() + time::Duration::minutes(5))
        .format(&time::format_description::well_known::Rfc3339)
        .map_err(|_| Error("command expiry unavailable"))?;
    let parameters = if kind == "start" {
        json!({"authorization_reference":parameter})
    } else {
        json!({"transaction_id":parameter})
    };
    let command = json!({"request_id":request_id,"resource":resource,
        "operation":{"kind":kind,"parameters":parameters},"expires_at":expires});
    let (status, body) = http
        .request(Method::POST, "/bridge/v1/commands", true, Some(&command))
        .await?;
    if status != 202 || body["request_id"] != request_id {
        return Err(Error("target command was not admitted"));
    }
    let status_url = body["status_url"]
        .as_str()
        .ok_or(Error("missing command result URL"))?;
    http.accepted(status_url, args, station, resource, &request_id)
        .await?;
    for _ in 0..150 {
        if let Some(snapshot) = http.snapshot(&station.id, &args.config.bridge_id).await?
            && let Some(id) =
                evidence::effect(&snapshot, &args.config, station, resource, old_ids, started)?
        {
            return Ok(id);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Err(Error(
        "native command accepted without observed charging effect",
    ))
}

fn station_url(base: &Url, id: &str, bridge_id: &str) -> Result<Url> {
    let mut path = base
        .join("/bridge/v1/stations/")
        .map_err(|_| Error("invalid station path"))?;
    path.path_segments_mut()
        .map_err(|()| Error("invalid station path"))?
        .pop_if_empty()
        .push(id);
    path.query_pairs_mut().append_pair("bridge_id", bridge_id);
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn station_url_uses_single_separator() {
        let base = Url::parse("http://127.0.0.1:19080/").expect("base URL");
        let path = station_url(&base, "station-a", "site-a").expect("station path");
        assert_eq!(path.path(), "/bridge/v1/stations/station-a");
        assert_eq!(path.query(), Some("bridge_id=site-a"));
    }
}
