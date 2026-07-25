//! A minimal S3-compatible object-storage client for `wovyr admin backup|restore`'s
//! remote destination (GA-002 §4.1, [A2-reliability-ha-dr.md](../../../docs/18-roadmap/v1.0/A2-reliability-ha-dr.md)) —
//! `wovyr admin backup <dest>` only ever wrote to a local filesystem path before this,
//! even though object storage was named as platform infrastructure in the Day-1
//! architecture docs.
//!
//! Hand-rolled AWS SigV4 request signing over `reqwest` + `hmac`/`sha2` (the same
//! crate/version `wovyr-events` already uses for webhook signing) rather than the
//! `aws-sdk-s3` crate: backup/restore only ever needs `PUT`/`GET`/`ListObjectsV2`
//! against one bucket, so the full SDK's credential-provider chain, retry policy,
//! and generated API surface for ~200 other AWS services would be pure dead
//! weight for this one CLI command. No new cargo feature gates this — `reqwest`
//! and `sha2` are already unconditional `wovyr-cli` dependencies, and `hmac` is
//! tiny, so there is no meaningful compile-time cost to keep it always available
//! and only reached at runtime when a destination/source starts with `s3://`.
//!
//! Path-style addressing (`{endpoint}/{bucket}/{key}`), not virtual-hosted-style
//! (`{bucket}.{endpoint}`) — the one addressing mode every S3-compatible store
//! (MinIO, Ceph RGW, real AWS S3 pointed at a regional endpoint) supports
//! uniformly, and the only one that works with an arbitrary user-supplied
//! `WOVYR_S3_ENDPOINT`.
//!
//! The SigV4 signing core (`sign_request` and its helpers) is verified against
//! reference values independently computed via .NET's `HMACSHA256`/`SHA256` (not
//! by this same Rust code) — see the `tests` module.

use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use std::path::Path;
use wovyr_common::{Error, Result};

type HmacSha256 = Hmac<Sha256>;

/// Connection details for one S3-compatible endpoint, sourced from environment
/// variables so `wovyr admin backup s3://bucket/prefix` needs no CLI flags beyond
/// the destination URI itself — the same "connection details via env var, target
/// via a URI argument" split every other Postgres/Qdrant/Redis backend in this
/// workspace uses.
pub struct S3Config {
    /// e.g. `https://s3.us-east-1.amazonaws.com` or `http://localhost:9000` (MinIO).
    pub endpoint: String,
    pub region: String,
    pub access_key_id: String,
    pub secret_access_key: String,
}

impl S3Config {
    /// `WOVYR_S3_ENDPOINT`, `WOVYR_S3_REGION` (default `us-east-1`),
    /// `WOVYR_S3_ACCESS_KEY_ID`, `WOVYR_S3_SECRET_ACCESS_KEY`.
    pub fn from_env() -> Result<Self> {
        Ok(Self {
            endpoint: require_env("WOVYR_S3_ENDPOINT")?,
            region: std::env::var("WOVYR_S3_REGION").unwrap_or_else(|_| "us-east-1".to_string()),
            access_key_id: require_env("WOVYR_S3_ACCESS_KEY_ID")?,
            secret_access_key: require_env("WOVYR_S3_SECRET_ACCESS_KEY")?,
        })
    }
}

fn require_env(var: &str) -> Result<String> {
    std::env::var(var).map_err(|_| {
        Error::config(format!(
            "{var} is not set (required for an s3:// backup/restore destination)"
        ))
    })
}

/// A parsed `s3://bucket[/prefix]` destination. `prefix` has no leading or
/// trailing `/` — object keys are always joined onto it explicitly via [`key`].
pub struct S3Uri {
    pub bucket: String,
    pub prefix: String,
}

impl S3Uri {
    pub fn is_s3(uri: &str) -> bool {
        uri.starts_with("s3://")
    }

    pub fn parse(uri: &str) -> Result<Self> {
        let rest = uri
            .strip_prefix("s3://")
            .ok_or_else(|| Error::invalid(format!("not an s3:// URI: {uri}")))?;
        let (bucket, prefix) = rest.split_once('/').unwrap_or((rest, ""));
        if bucket.is_empty() {
            return Err(Error::invalid(format!("s3:// URI missing a bucket: {uri}")));
        }
        Ok(Self {
            bucket: bucket.to_string(),
            prefix: prefix.trim_matches('/').to_string(),
        })
    }

    /// Join a backup-relative path (e.g. `kms/root.key`) onto this URI's prefix.
    pub fn key(&self, relative: &str) -> String {
        if self.prefix.is_empty() {
            relative.to_string()
        } else {
            format!("{}/{relative}", self.prefix)
        }
    }
}

/// A minimal S3-compatible client: enough for `wovyr admin backup|restore` to
/// `put`/`get`/`list` objects in one bucket.
pub struct S3Client {
    config: S3Config,
    bucket: String,
    http: reqwest::Client,
}

impl S3Client {
    pub fn new(config: S3Config, bucket: String) -> Self {
        Self {
            config,
            bucket,
            http: reqwest::Client::new(),
        }
    }

    pub async fn put(&self, key: &str, bytes: Vec<u8>) -> Result<()> {
        let canonical_uri = self.canonical_uri(key);
        let url = self.object_url(&canonical_uri);
        let host = self.host()?;
        let (amz_date, authorization, payload_hash) = sign_request(
            &self.config,
            "PUT",
            &canonical_uri,
            "",
            &bytes,
            &host,
            current_unix_secs(),
        );

        let resp = self
            .http
            .put(&url)
            .header("host", &host)
            .header("x-amz-date", &amz_date)
            .header("x-amz-content-sha256", &payload_hash)
            .header("authorization", authorization)
            .body(bytes)
            .send()
            .await
            .map_err(|e| Error::config(format!("s3 put `{key}`: {e}")))?;
        ensure_success(resp).await?;
        Ok(())
    }

    pub async fn get(&self, key: &str) -> Result<Vec<u8>> {
        let canonical_uri = self.canonical_uri(key);
        let url = self.object_url(&canonical_uri);
        let host = self.host()?;
        let (amz_date, authorization, payload_hash) = sign_request(
            &self.config,
            "GET",
            &canonical_uri,
            "",
            b"",
            &host,
            current_unix_secs(),
        );

        let resp = self
            .http
            .get(&url)
            .header("host", &host)
            .header("x-amz-date", &amz_date)
            .header("x-amz-content-sha256", &payload_hash)
            .header("authorization", authorization)
            .send()
            .await
            .map_err(|e| Error::config(format!("s3 get `{key}`: {e}")))?;
        let resp = ensure_success(resp).await?;
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| Error::config(format!("s3 get `{key}` body: {e}")))?;
        Ok(bytes.to_vec())
    }

    /// List every key under `prefix` (ListObjectsV2, paginated via
    /// `continuation-token` until `IsTruncated` is false).
    pub async fn list(&self, prefix: &str) -> Result<Vec<String>> {
        let mut keys = Vec::new();
        let mut continuation: Option<String> = None;
        loop {
            let mut params: Vec<(&str, String)> = vec![
                ("list-type", "2".to_string()),
                ("prefix", prefix.to_string()),
            ];
            if let Some(token) = &continuation {
                params.push(("continuation-token", token.clone()));
            }
            params.sort_by(|a, b| a.0.cmp(b.0));
            let canonical_query = params
                .iter()
                .map(|(k, v)| format!("{}={}", uri_encode(k, true), uri_encode(v, true)))
                .collect::<Vec<_>>()
                .join("&");

            let canonical_uri = format!("/{}", uri_encode(&self.bucket, false));
            let host = self.host()?;
            let url = format!(
                "{}{canonical_uri}?{canonical_query}",
                self.config.endpoint.trim_end_matches('/')
            );
            let (amz_date, authorization, payload_hash) = sign_request(
                &self.config,
                "GET",
                &canonical_uri,
                &canonical_query,
                b"",
                &host,
                current_unix_secs(),
            );

            let resp = self
                .http
                .get(&url)
                .header("host", &host)
                .header("x-amz-date", &amz_date)
                .header("x-amz-content-sha256", &payload_hash)
                .header("authorization", authorization)
                .send()
                .await
                .map_err(|e| Error::config(format!("s3 list `{prefix}`: {e}")))?;
            let resp = ensure_success(resp).await?;
            let body = resp
                .text()
                .await
                .map_err(|e| Error::config(format!("s3 list `{prefix}` body: {e}")))?;

            keys.extend(extract_xml_tag_values(&body, "Key"));
            continuation = extract_xml_tag_values(&body, "NextContinuationToken")
                .into_iter()
                .next();
            let is_truncated = extract_xml_tag_values(&body, "IsTruncated")
                .into_iter()
                .next()
                .as_deref()
                == Some("true");
            if !is_truncated || continuation.is_none() {
                break;
            }
        }
        Ok(keys)
    }

    fn canonical_uri(&self, key: &str) -> String {
        format!(
            "/{}/{}",
            uri_encode(&self.bucket, false),
            uri_encode(key, false)
        )
    }

    fn object_url(&self, canonical_uri: &str) -> String {
        format!(
            "{}{canonical_uri}",
            self.config.endpoint.trim_end_matches('/')
        )
    }

    fn host(&self) -> Result<String> {
        let url = reqwest::Url::parse(&self.config.endpoint).map_err(|e| {
            Error::config(format!(
                "invalid WOVYR_S3_ENDPOINT `{}`: {e}",
                self.config.endpoint
            ))
        })?;
        let host = url.host_str().ok_or_else(|| {
            Error::config(format!(
                "WOVYR_S3_ENDPOINT `{}` has no host",
                self.config.endpoint
            ))
        })?;
        Ok(match url.port() {
            Some(port) => format!("{host}:{port}"),
            None => host.to_string(),
        })
    }
}

async fn ensure_success(resp: reqwest::Response) -> Result<reqwest::Response> {
    if resp.status().is_success() {
        Ok(resp)
    } else {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        Err(Error::config(format!(
            "s3 request failed ({status}): {body}"
        )))
    }
}

fn current_unix_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Sign one S3 request (AWS Signature Version 4). Pure and deterministic given
/// `unix_secs` — real callers pass [`current_unix_secs`]; tests pass a fixed
/// value. Returns `(x-amz-date, Authorization header, x-amz-content-sha256)` —
/// all three must be sent as request headers (the last is itself a signed
/// header, per SigV4's convention of covering the payload hash).
fn sign_request(
    config: &S3Config,
    method: &str,
    canonical_uri: &str,
    canonical_query: &str,
    payload: &[u8],
    host: &str,
    unix_secs: u64,
) -> (String, String, String) {
    let payload_hash = sha256_hex(payload);
    let (amz_date, date_stamp) = unix_to_amz_date(unix_secs);

    let mut headers = [
        ("host", host),
        ("x-amz-content-sha256", payload_hash.as_str()),
        ("x-amz-date", amz_date.as_str()),
    ];
    headers.sort_by(|a, b| a.0.cmp(b.0));
    let canonical_headers: String = headers.iter().map(|(k, v)| format!("{k}:{v}\n")).collect();
    let signed_headers = headers
        .iter()
        .map(|(k, _)| *k)
        .collect::<Vec<_>>()
        .join(";");

    let canonical_request = format!(
        "{method}\n{canonical_uri}\n{canonical_query}\n{canonical_headers}\n{signed_headers}\n{payload_hash}"
    );
    let credential_scope = format!("{date_stamp}/{}/s3/aws4_request", config.region);
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{amz_date}\n{credential_scope}\n{}",
        sha256_hex(canonical_request.as_bytes())
    );

    let k_secret = format!("AWS4{}", config.secret_access_key);
    let k_date = hmac_sha256(k_secret.as_bytes(), date_stamp.as_bytes());
    let k_region = hmac_sha256(&k_date, config.region.as_bytes());
    let k_service = hmac_sha256(&k_region, b"s3");
    let k_signing = hmac_sha256(&k_service, b"aws4_request");
    let signature = hex::encode(hmac_sha256(&k_signing, string_to_sign.as_bytes()));

    let authorization = format!(
        "AWS4-HMAC-SHA256 Credential={}/{credential_scope},SignedHeaders={signed_headers},Signature={signature}",
        config.access_key_id
    );

    (amz_date, authorization, payload_hash)
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn hmac_sha256(key: &[u8], msg: &[u8]) -> Vec<u8> {
    let mut mac =
        <HmacSha256 as Mac>::new_from_slice(key).expect("HMAC accepts a key of any length");
    mac.update(msg);
    mac.finalize().into_bytes().to_vec()
}

/// Percent-encode per SigV4's rules: every byte except the unreserved set
/// (`A-Za-z0-9-_.~`) becomes `%XX` (uppercase hex). `/` is left literal for a
/// canonical *URI path* (`encode_slash = false`) but percent-encoded for a
/// canonical *query string* value (`encode_slash = true`) — the one place
/// SigV4's path- and query-encoding rules diverge.
fn uri_encode(s: &str, encode_slash: bool) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        let c = b as char;
        if c.is_ascii_alphanumeric()
            || matches!(c, '-' | '_' | '.' | '~')
            || (c == '/' && !encode_slash)
        {
            out.push(c);
        } else {
            out.push('%');
            out.push_str(&format!("{b:02X}"));
        }
    }
    out
}

/// Convert a Unix timestamp (UTC, no leap seconds) into SigV4's
/// `x-amz-date` (`YYYYMMDDTHHMMSSZ`) and its `YYYYMMDD` date-stamp prefix.
/// Civil-date math via Howard Hinnant's `civil_from_days` algorithm
/// (<http://howardhinnant.github.io/date_algorithms.html>) — the same
/// dependency-free approach `wovyr-server`'s `hardening.rs` uses for HTTP-date
/// formatting, avoiding a `chrono` dependency for one date conversion. Verified
/// against three independently-computed (.NET `DateTimeOffset`) reference
/// points, including a leap day — see `tests::unix_to_amz_date_matches_a_net_computed_reference`.
fn unix_to_amz_date(unix_secs: u64) -> (String, String) {
    let days = (unix_secs / 86400) as i64;
    let secs_of_day = unix_secs % 86400;
    let hour = secs_of_day / 3600;
    let min = (secs_of_day % 3600) / 60;
    let sec = secs_of_day % 60;

    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let y = if m <= 2 { y + 1 } else { y };

    let date_stamp = format!("{y:04}{m:02}{d:02}");
    let amz_date = format!("{date_stamp}T{hour:02}{min:02}{sec:02}Z");
    (amz_date, date_stamp)
}

/// Pull every `<tag>...</tag>` value out of `xml` (flat scan — ListObjectsV2's
/// response never nests one of these tags inside another instance of itself, so
/// this avoids a full XML-parser dependency for one response shape).
fn extract_xml_tag_values(xml: &str, tag: &str) -> Vec<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some(start) = rest.find(&open) {
        let after_open = &rest[start + open.len()..];
        match after_open.find(&close) {
            Some(end) => {
                out.push(xml_unescape(&after_open[..end]));
                rest = &after_open[end + close.len()..];
            }
            None => break,
        }
    }
    out
}

/// Unescape the five XML predefined entities. A single left-to-right pass
/// (rather than chained `str::replace` calls) so an entity produced by
/// unescaping one match — e.g. a key literally containing the text
/// `&amp;lt;` — is never itself re-unescaped.
fn xml_unescape(s: &str) -> String {
    const ENTITIES: [(&str, char); 5] = [
        ("&amp;", '&'),
        ("&lt;", '<'),
        ("&gt;", '>'),
        ("&quot;", '"'),
        ("&apos;", '\''),
    ];
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while !rest.is_empty() {
        if rest.starts_with('&') {
            if let Some((pat, ch)) = ENTITIES.iter().find(|(pat, _)| rest.starts_with(pat)) {
                out.push(*ch);
                rest = &rest[pat.len()..];
                continue;
            }
        }
        let mut chars = rest.chars();
        let c = chars.next().expect("rest is non-empty");
        out.push(c);
        rest = chars.as_str();
    }
    out
}

/// Upload every file under `dir` into `uri`'s bucket/prefix, preserving the
/// same relative-path structure `wovyr admin backup`'s local manifest already
/// records. Returns the number of files uploaded.
pub async fn upload_dir(config: S3Config, uri: &S3Uri, dir: &Path) -> Result<usize> {
    let client = S3Client::new(config, uri.bucket.clone());
    let files = collect_files(dir, dir)?;
    for (rel, path) in &files {
        let bytes = std::fs::read(path)?;
        client.put(&uri.key(rel), bytes).await?;
    }
    Ok(files.len())
}

/// Download every object under `uri`'s bucket/prefix into `dir`, restoring the
/// same relative-path structure `upload_dir` used. Returns the number of files
/// downloaded.
pub async fn download_dir(config: S3Config, uri: &S3Uri, dir: &Path) -> Result<usize> {
    let client = S3Client::new(config, uri.bucket.clone());
    let keys = client.list(&uri.prefix).await?;
    let mut count = 0;
    for key in keys {
        let rel = if uri.prefix.is_empty() {
            key.as_str()
        } else {
            key.strip_prefix(&format!("{}/", uri.prefix))
                .unwrap_or(&key)
        };
        let bytes = client.get(&key).await?;
        let out_path = dir.join(rel);
        if let Some(parent) = out_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&out_path, &bytes)?;
        count += 1;
    }
    Ok(count)
}

/// Every regular file under `dir`, as `(relative "/"-joined path, absolute path)`
/// pairs — the same relative-path convention `admin.rs`'s local `backup_dir`
/// uses, kept as a small independent helper here rather than widening that
/// module's private `relative_slash_path`'s visibility for one caller.
fn collect_files(root: &Path, dir: &Path) -> Result<Vec<(String, std::path::PathBuf)>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            out.extend(collect_files(root, &path)?);
            continue;
        }
        let rel = path
            .strip_prefix(root)
            .expect("path is under root")
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/");
        out.push((rel, path));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- SigV4 signing: verified against .NET-computed reference values ------------
    //
    // Every constant below (payload hash, canonical request, string-to-sign,
    // signature) was independently computed via PowerShell's
    // `[System.Security.Cryptography.SHA256]`/`HMACSHA256` for this exact fixed
    // input — not derived from this Rust code — so a match is real, external
    // verification of the signing implementation, not the implementation
    // checking itself.

    fn reference_config() -> S3Config {
        S3Config {
            endpoint: "https://s3.example.com".to_string(),
            region: "us-east-1".to_string(),
            access_key_id: "AKIDEXAMPLE".to_string(),
            secret_access_key: "wJalrXUtnFEMISECRETEXAMPLEKEY".to_string(),
        }
    }

    #[test]
    fn sign_request_matches_a_net_computed_reference() {
        let config = reference_config();
        // 2013-05-24T00:00:00Z (verified: `[DateTimeOffset]::FromUnixTimeSeconds(1369353600)`).
        let unix_secs = 1_369_353_600;

        let (amz_date, authorization, payload_hash) = sign_request(
            &config,
            "GET",
            "/examplebucket/test.txt",
            "",
            b"",
            "s3.example.com",
            unix_secs,
        );

        assert_eq!(amz_date, "20130524T000000Z");
        assert_eq!(
            payload_hash,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            authorization,
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20130524/us-east-1/s3/aws4_request,\
             SignedHeaders=host;x-amz-content-sha256;x-amz-date,\
             Signature=a8080dd2e02c71b48480dc0388bc2cf3a466aa9c608eded88b38af89afe727d6"
        );
    }

    #[test]
    fn sign_request_changes_signature_when_any_input_changes() {
        let config = reference_config();
        let base = sign_request(
            &config,
            "GET",
            "/examplebucket/test.txt",
            "",
            b"",
            "s3.example.com",
            1_369_353_600,
        );

        let different_method = sign_request(
            &config,
            "PUT",
            "/examplebucket/test.txt",
            "",
            b"",
            "s3.example.com",
            1_369_353_600,
        );
        let different_payload = sign_request(
            &config,
            "GET",
            "/examplebucket/test.txt",
            "",
            b"changed",
            "s3.example.com",
            1_369_353_600,
        );
        let different_time = sign_request(
            &config,
            "GET",
            "/examplebucket/test.txt",
            "",
            b"",
            "s3.example.com",
            1_369_353_601,
        );

        assert_ne!(base.1, different_method.1);
        assert_ne!(base.1, different_payload.1);
        assert_ne!(base.1, different_time.1);
    }

    // --- Civil-date math: verified against .NET-computed reference values ----------

    #[test]
    fn unix_to_amz_date_matches_a_net_computed_reference() {
        // Each pair independently computed via .NET's `DateTimeOffset`, including
        // the Unix epoch itself and a leap day (the trickiest civil-date case).
        let cases = [
            (0u64, "19700101T000000Z"),
            (1, "19700101T000001Z"),
            (1_369_353_600, "20130524T000000Z"),
            (1_709_210_096, "20240229T123456Z"),
            (946_684_799, "19991231T235959Z"),
        ];
        for (unix_secs, expected) in cases {
            let (amz_date, _) = unix_to_amz_date(unix_secs);
            assert_eq!(amz_date, expected, "for unix_secs={unix_secs}");
        }
    }

    // --- URI encoding ---------------------------------------------------------------

    #[test]
    fn uri_encode_leaves_unreserved_characters_alone() {
        assert_eq!(uri_encode("abcXYZ019-_.~", false), "abcXYZ019-_.~");
    }

    #[test]
    fn uri_encode_path_style_keeps_slashes_literal() {
        assert_eq!(uri_encode("kms/root.key", false), "kms/root.key");
    }

    #[test]
    fn uri_encode_query_style_encodes_slashes() {
        assert_eq!(uri_encode("kms/root.key", true), "kms%2Froot.key");
    }

    #[test]
    fn uri_encode_percent_encodes_spaces_and_special_characters() {
        assert_eq!(uri_encode("a b", false), "a%20b");
        assert_eq!(uri_encode("a=b&c", true), "a%3Db%26c");
    }

    // --- S3Uri ------------------------------------------------------------------------

    #[test]
    fn s3_uri_parses_bucket_and_prefix() {
        let uri = S3Uri::parse("s3://my-bucket/backups/prod").unwrap();
        assert_eq!(uri.bucket, "my-bucket");
        assert_eq!(uri.prefix, "backups/prod");
        assert_eq!(uri.key("kms/root.key"), "backups/prod/kms/root.key");
    }

    #[test]
    fn s3_uri_with_no_prefix_joins_the_bare_relative_path() {
        let uri = S3Uri::parse("s3://my-bucket").unwrap();
        assert_eq!(uri.bucket, "my-bucket");
        assert_eq!(uri.prefix, "");
        assert_eq!(uri.key("kms/root.key"), "kms/root.key");
    }

    #[test]
    fn s3_uri_rejects_a_missing_bucket() {
        assert!(S3Uri::parse("s3://").is_err());
        assert!(S3Uri::parse("s3:///prefix").is_err());
    }

    #[test]
    fn s3_uri_rejects_a_non_s3_scheme() {
        assert!(S3Uri::parse("/local/path").is_err());
        assert!(S3Uri::parse("gs://bucket/prefix").is_err());
    }

    #[test]
    fn is_s3_detects_only_the_s3_scheme() {
        assert!(S3Uri::is_s3("s3://bucket/prefix"));
        assert!(!S3Uri::is_s3("/local/path"));
        assert!(!S3Uri::is_s3("C:\\backups"));
    }

    // --- ListObjectsV2 XML scanning --------------------------------------------------

    #[test]
    fn extract_xml_tag_values_finds_every_occurrence() {
        let xml = "<ListBucketResult><Contents><Key>a/b.txt</Key></Contents>\
                    <Contents><Key>c.txt</Key></Contents></ListBucketResult>";
        assert_eq!(
            extract_xml_tag_values(xml, "Key"),
            vec!["a/b.txt".to_string(), "c.txt".to_string()]
        );
    }

    #[test]
    fn extract_xml_tag_values_unescapes_entities() {
        let xml = "<Key>a&amp;b&lt;c&gt;d&quot;e&apos;f</Key>";
        assert_eq!(
            extract_xml_tag_values(xml, "Key"),
            vec!["a&b<c>d\"e'f".to_string()]
        );
    }

    #[test]
    fn xml_unescape_does_not_double_unescape() {
        // A key literally containing the text "&lt;" (already escaped once by the
        // server as "&amp;lt;") must come back as the literal text "&lt;", not "<".
        assert_eq!(xml_unescape("&amp;lt;"), "&lt;");
    }

    // --- collect_files ----------------------------------------------------------------

    #[test]
    fn collect_files_walks_nested_directories() {
        let dir = std::env::temp_dir().join(format!(
            "wovyr_cli_s3_collect_test_{}_{}",
            std::process::id(),
            current_unix_secs()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("a/b")).unwrap();
        std::fs::write(dir.join("top.txt"), b"1").unwrap();
        std::fs::write(dir.join("a/mid.txt"), b"2").unwrap();
        std::fs::write(dir.join("a/b/deep.txt"), b"3").unwrap();

        let mut files: Vec<String> = collect_files(&dir, &dir)
            .unwrap()
            .into_iter()
            .map(|(rel, _)| rel)
            .collect();
        files.sort();
        assert_eq!(files, vec!["a/b/deep.txt", "a/mid.txt", "top.txt"]);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
