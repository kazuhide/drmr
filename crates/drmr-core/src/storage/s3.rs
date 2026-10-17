//! `S3StorageBackend` (`storage`'s boto3 client): region-pinned to `ap-northeast-1`, the
//! bucket's location verified before any transfer, every request bounded by the connect and
//! per-read timeout and retried with capped exponential backoff and jitter on the transient
//! failures botocore would raise (`SlowDown`, `Throttling`, `RequestTimeout`, `InternalError`,
//! `ServiceUnavailable`, any 5xx, connect / read timeouts, closed connections, truncated
//! bodies). Requests are SigV4-presigned (`rusty-s3`) and sent with `ureq`.
//!
//! Credentials come from the environment (`AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`,
//! `AWS_SESSION_TOKEN`) or the standard profile files (`AWS_SHARED_CREDENTIALS_FILE`,
//! `AWS_CONFIG_FILE`, `AWS_PROFILE`), never from a flag, a work list or a log; no error
//! carries a credential, a signed URL or a provider message.

use super::{
    StorageBackend, StorageError, StorageLocation, TransferPolicy, confined, owned, parallel,
    transfer_file,
};
use crate::batch_support::{uniform, unquote_plus};
use crate::storage::ObjectRef;
use rusty_s3::{Bucket, Credentials, S3Action, UrlStyle};
use std::fmt;
use std::fs::File;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The only region the batch may use (the configured regional transfer boundary).
pub const REGION: &str = "ap-northeast-1";
/// How long a presigned request stays valid; requests are sent as soon as they are signed.
const SIGNATURE_LIFETIME: Duration = Duration::from_mins(15);
/// Error bodies are read up to this size for their `Code`.
const MAX_ERROR_BODY: u64 = 64 * 1024;
/// Listing and location responses are read up to this size.
const MAX_XML_BODY: u64 = 16 * 1024 * 1024;
/// The codes `_call` retries besides every 5xx.
const RETRYABLE_CODES: &[&str] = &[
    "SlowDown",
    "Throttling",
    "RequestTimeout",
    "InternalError",
    "ServiceUnavailable",
];

/// `S3StorageBackend`. `Debug` hides the bucket and the client (hidden debug fields).
pub struct S3StorageBackend {
    bucket: String,
    region: String,
    policy: TransferPolicy,
    output_prefix: String,
    endpoint: Option<String>,
    client: Client,
}

impl fmt::Debug for S3StorageBackend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("S3StorageBackend")
            .field("region", &self.region)
            .field("policy", &self.policy)
            .field("output_prefix", &self.output_prefix)
            .finish_non_exhaustive()
    }
}

/// An explicit endpoint and credentials (Python's injected boto3 client). `Debug` shows the
/// endpoint only.
#[derive(Clone)]
pub struct S3Access {
    /// The endpoint URL (path-style addressing).
    pub endpoint: String,
    /// The access key id.
    pub access_key: String,
    /// The secret access key.
    pub secret_key: String,
    /// The session token, if any.
    pub session_token: Option<String>,
}

impl fmt::Debug for S3Access {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("S3Access")
            .field("endpoint", &self.endpoint)
            .finish_non_exhaustive()
    }
}

struct Client {
    agent: ureq::Agent,
    bucket: Bucket,
    credentials: Option<Credentials>,
}

/// What one request attempt ended with, as `_call` tells botocore's exceptions apart.
enum CallError {
    /// An error response (`ClientError`): its S3 error code (the status for a body-less
    /// response) and status.
    Status { status: u16, code: String },
    /// A transport failure; retryable for timeouts, closed connections and truncated bodies.
    Transport { retryable: bool },
    /// A `StorageError` raised inside the request (byte budget, length mismatch).
    Storage(StorageError),
    /// Anything else: a local file error, an unparsable response, no credentials.
    Other,
}

impl CallError {
    fn retryable(&self) -> bool {
        match self {
            CallError::Status { status, code } => {
                *status >= 500 || RETRYABLE_CODES.contains(&code.as_str())
            }
            CallError::Transport { retryable } => *retryable,
            CallError::Storage(error) => error.is_retryable(),
            CallError::Other => false,
        }
    }
}

impl S3StorageBackend {
    /// `S3StorageBackend(bucket, region)` with the default policy and the `output` prefix.
    pub fn new(bucket: &str, region: &str) -> Result<S3StorageBackend, StorageError> {
        S3StorageBackend::with(bucket, region, TransferPolicy::default(), "output", None)
    }

    /// `S3StorageBackend(bucket, region, policy, output_prefix)`, optionally against an
    /// explicit endpoint (otherwise `AWS_ENDPOINT_URL_S3` / `AWS_ENDPOINT_URL`, as botocore
    /// reads them, or the regional AWS endpoint), with the environment's or the profile files'
    /// credentials. The region is checked before any client exists; the bucket's location is
    /// verified before the backend is returned.
    pub fn with(
        bucket: &str,
        region: &str,
        policy: TransferPolicy,
        output_prefix: &str,
        endpoint: Option<&str>,
    ) -> Result<S3StorageBackend, StorageError> {
        S3StorageBackend::build(bucket, region, policy, output_prefix, endpoint, None)
    }

    /// The backend against an explicit endpoint with explicit credentials: Python's injected
    /// `_client` (a test double, a non-AWS endpoint). A worker process rebuilds it from the
    /// endpoint and its own environment's credentials, never from these.
    pub fn with_access(
        bucket: &str,
        region: &str,
        policy: TransferPolicy,
        output_prefix: &str,
        access: &S3Access,
    ) -> Result<S3StorageBackend, StorageError> {
        let endpoint = Some(access.endpoint.as_str());
        S3StorageBackend::build(
            bucket,
            region,
            policy,
            output_prefix,
            endpoint,
            Some(access),
        )
    }

    fn build(
        bucket: &str,
        region: &str,
        policy: TransferPolicy,
        output_prefix: &str,
        endpoint: Option<&str>,
        access: Option<&S3Access>,
    ) -> Result<S3StorageBackend, StorageError> {
        if region != REGION {
            return Err(StorageError::new("storage-region-invalid"));
        }
        let policy = policy.validate()?;
        ObjectRef::new(output_prefix)?;
        let client = Client::new(bucket, region, &policy, endpoint, access)
            .ok_or_else(|| StorageError::new("storage-client-unavailable"))?;
        let backend = S3StorageBackend {
            bucket: bucket.to_string(),
            region: region.to_string(),
            policy,
            output_prefix: output_prefix.to_string(),
            endpoint: endpoint.map(str::to_string),
            client,
        };
        let location = backend.call(|| backend.client.bucket_location())?;
        if location.as_deref() != Some(region) {
            return Err(StorageError::new("storage-region-invalid"));
        }
        Ok(backend)
    }

    /// `_call`: up to `max_attempts` attempts; a non-retryable failure is `storage-failed` at
    /// once, an exhausted retryable one `storage-transient` (retryable). Between attempts the
    /// jitter is `uniform(0, min(backoff_max_s, backoff_base_s * 2**attempt))`.
    fn call<T>(&self, function: impl Fn() -> Result<T, CallError>) -> Result<T, StorageError> {
        let attempts = self.policy.max_attempts;
        let mut attempt: i32 = 0;
        loop {
            let error = match function() {
                Ok(value) => return Ok(value),
                Err(error) => error,
            };
            let retryable = error.retryable();
            if !retryable || i64::from(attempt) + 1 >= attempts {
                return Err(if retryable {
                    StorageError::retryable("storage-transient")
                } else {
                    StorageError::new("storage-failed")
                });
            }
            let backoff = self
                .policy
                .backoff_max_s
                .min(self.policy.backoff_base_s * 2f64.powi(attempt));
            std::thread::sleep(Duration::try_from_secs_f64(uniform(backoff)).unwrap_or_default());
            attempt += 1;
        }
    }

    fn head(&self, object: &ObjectRef) -> Result<u64, StorageError> {
        self.call(|| self.client.head(object.key()))
    }

    fn download(&self, object: &ObjectRef, target: &Path) -> Result<(), StorageError> {
        let parent = target.parent().unwrap_or_else(|| Path::new("."));
        std::fs::create_dir_all(parent).map_err(|_| StorageError::new("storage-failed"))?;
        self.call(|| {
            let response = self.client.get(object.key())?;
            let declared = content_length(&response);
            let mut temporary = transfer_file(parent).map_err(|_| CallError::Other)?;
            let used = stream_body(
                response.into_reader(),
                temporary.as_file_mut(),
                self.policy.object_cap(),
            )?;
            temporary
                .as_file()
                .sync_all()
                .map_err(|_| CallError::Other)?;
            match declared {
                None => return Err(CallError::Other),
                Some(length) if length != used => {
                    return Err(CallError::Storage(StorageError::retryable(
                        "storage-transient",
                    )));
                }
                Some(_) => {}
            }
            temporary.persist(target).map_err(|_| CallError::Other)?;
            Ok(())
        })
    }

    fn upload(&self, source: &Path, object: &ObjectRef) -> Result<(), StorageError> {
        owned(object, &self.output_prefix)?;
        if source.is_symlink() {
            return Err(StorageError::new("storage-object-invalid"));
        }
        let size = std::fs::metadata(source)
            .map_err(|_| StorageError::new("storage-failed"))?
            .len();
        if size > self.policy.object_cap() {
            return Err(StorageError::new("storage-object-invalid"));
        }
        self.call(|| {
            let file = File::open(source).map_err(|_| CallError::Other)?;
            let length = file.metadata().map_err(|_| CallError::Other)?.len();
            self.client.put(object.key(), file, length)
        })
    }
}

impl StorageBackend for S3StorageBackend {
    fn exists(&self, object: &ObjectRef) -> Result<bool, StorageError> {
        self.call(|| match self.client.head(object.key()) {
            Ok(_) => Ok(true),
            Err(CallError::Status { code, .. })
                if matches!(code.as_str(), "404" | "NoSuchKey" | "NotFound") =>
            {
                Ok(false)
            }
            Err(error) => Err(error),
        })
    }

    fn object_size(&self, object: &ObjectRef) -> Result<u64, StorageError> {
        self.head(object)
    }

    fn list_prefix(&self, prefix: &ObjectRef) -> Result<Vec<ObjectRef>, StorageError> {
        let listed = format!("{}/", prefix.key().trim_end_matches('/'));
        let mut refs = Vec::new();
        let mut token: Option<String> = None;
        loop {
            let page = self.call(|| {
                self.client
                    .list(&listed, self.policy.batch_size, token.as_deref())
            })?;
            for key in page.keys {
                if !key.ends_with('/') {
                    refs.push(ObjectRef::new(key)?);
                }
            }
            if refs.len() > self.policy.object_budget() {
                return Err(StorageError::new("storage-object-budget"));
            }
            if !page.truncated {
                return Ok(refs);
            }
            match page.next_token.filter(|next| !next.is_empty()) {
                Some(next) => token = Some(next),
                None => return Err(StorageError::new("storage-list-invalid")),
            }
        }
    }

    fn get_many(&self, refs: &[ObjectRef], dest_root: &Path) -> Result<Vec<PathBuf>, StorageError> {
        let mut sizes = Vec::with_capacity(refs.len());
        for object in refs {
            sizes.push(self.head(object)?);
        }
        let total = sizes
            .iter()
            .fold(0u64, |sum, size| sum.saturating_add(*size));
        if total > self.policy.asset_cap() || sizes.iter().any(|s| *s > self.policy.object_cap()) {
            return Err(StorageError::new("storage-byte-budget"));
        }
        let download = |object: &ObjectRef| -> Result<PathBuf, StorageError> {
            let target = confined(dest_root, object)?;
            self.download(object, &target)?;
            Ok(target)
        };
        parallel(refs, download, &self.policy)
    }

    fn put_many(&self, items: &[(PathBuf, ObjectRef)]) -> Result<(), StorageError> {
        let upload = |(source, object): &(PathBuf, ObjectRef)| self.upload(source, object);
        parallel(items, upload, &self.policy).map(|_| ())
    }

    fn delete_prefix(&self, prefix: &ObjectRef) -> Result<(), StorageError> {
        owned(prefix, &self.output_prefix)?;
        let refs = self.list_prefix(prefix)?;
        for chunk in refs.chunks(self.policy.chunk()) {
            let keys: Vec<&str> = chunk.iter().map(ObjectRef::key).collect();
            let errors = self.call(|| self.client.delete(&keys))?;
            if errors {
                return Err(StorageError::new("storage-delete-failed"));
            }
        }
        Ok(())
    }

    fn bucket(&self) -> Option<&str> {
        Some(&self.bucket)
    }

    fn policy(&self) -> Option<TransferPolicy> {
        Some(self.policy)
    }

    fn location(&self) -> Option<StorageLocation> {
        Some(StorageLocation::S3 {
            bucket: self.bucket.clone(),
            region: self.region.clone(),
            endpoint: self.endpoint.clone(),
        })
    }
}

/// One page of `list_objects_v2`.
struct ListPage {
    keys: Vec<String>,
    truncated: bool,
    next_token: Option<String>,
}

impl Client {
    fn new(
        bucket: &str,
        region: &str,
        policy: &TransferPolicy,
        endpoint: Option<&str>,
        access: Option<&S3Access>,
    ) -> Option<Client> {
        let explicit = endpoint.map(str::to_string).or_else(configured_endpoint);
        let (url, style) = if let Some(url) = explicit {
            (url, UrlStyle::Path)
        } else if virtual_host_compatible(bucket) {
            (
                format!("https://s3.{region}.amazonaws.com"),
                UrlStyle::VirtualHost,
            )
        } else {
            (format!("https://s3.{region}.amazonaws.com"), UrlStyle::Path)
        };
        let endpoint = url.parse().ok()?;
        let bucket = Bucket::new(endpoint, style, bucket.to_string(), region.to_string()).ok()?;
        let credentials = match access {
            Some(access) => Some(match &access.session_token {
                Some(token) => {
                    Credentials::new_with_token(&access.access_key, &access.secret_key, token)
                }
                None => Credentials::new(&access.access_key, &access.secret_key),
            }),
            None => credentials().ok()?,
        };
        let timeout = Duration::try_from_secs_f64(policy.per_object_timeout_s).ok()?;
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(timeout)
            .timeout_read(timeout)
            .timeout_write(timeout)
            .redirects(0)
            .build();
        Some(Client {
            agent,
            bucket,
            credentials,
        })
    }

    fn signed(&self) -> Result<&Credentials, CallError> {
        // botocore signs at request time: no credentials is a non-retryable NoCredentialsError.
        self.credentials.as_ref().ok_or(CallError::Other)
    }

    fn bucket_location(&self) -> Result<Option<String>, CallError> {
        let mut action = self.bucket.get_object(Some(self.signed()?), "");
        action.query_mut().insert("location", "");
        let response = send(
            self.agent.get(action.sign(SIGNATURE_LIFETIME).as_str()),
            None,
        )?;
        let text = read_text(response, MAX_XML_BODY)?;
        let document = roxmltree::Document::parse(&text).map_err(|_| CallError::Other)?;
        let root = document.root_element();
        if root.tag_name().name() != "LocationConstraint" {
            return Err(CallError::Other);
        }
        Ok(Some(element_text(root)).filter(|text| !text.is_empty()))
    }

    fn head(&self, key: &str) -> Result<u64, CallError> {
        let action = self.bucket.head_object(Some(self.signed()?), key);
        let response = send(
            self.agent.head(action.sign(SIGNATURE_LIFETIME).as_str()),
            None,
        )?;
        content_length(&response).ok_or(CallError::Other)
    }

    fn get(&self, key: &str) -> Result<ureq::Response, CallError> {
        let action = self.bucket.get_object(Some(self.signed()?), key);
        send(
            self.agent.get(action.sign(SIGNATURE_LIFETIME).as_str()),
            None,
        )
    }

    fn put(&self, key: &str, file: File, length: u64) -> Result<(), CallError> {
        let action = self.bucket.put_object(Some(self.signed()?), key);
        let request = self
            .agent
            .put(action.sign(SIGNATURE_LIFETIME).as_str())
            .set("Content-Length", &length.to_string());
        send(request, Some(Body::File(file))).map(|_| ())
    }

    fn list(
        &self,
        prefix: &str,
        max_keys: i64,
        token: Option<&str>,
    ) -> Result<ListPage, CallError> {
        let mut action = self.bucket.list_objects_v2(Some(self.signed()?));
        action.with_prefix(prefix);
        action.with_max_keys(usize::try_from(max_keys).unwrap_or(1000));
        if let Some(token) = token {
            action.with_continuation_token(token);
        }
        let response = send(
            self.agent.get(action.sign(SIGNATURE_LIFETIME).as_str()),
            None,
        )?;
        let text = read_text(response, MAX_XML_BODY)?;
        parse_list(&text).ok_or(CallError::Other)
    }

    fn delete(&self, keys: &[&str]) -> Result<bool, CallError> {
        let objects: Vec<rusty_s3::actions::ObjectIdentifier> = keys
            .iter()
            .map(|key| rusty_s3::actions::ObjectIdentifier::new((*key).to_string()))
            .collect();
        let mut action = self
            .bucket
            .delete_objects(Some(self.signed()?), objects.iter());
        action.set_quiet(true);
        let url = action.sign(SIGNATURE_LIFETIME);
        let (body, md5) = action.body_with_md5();
        let request = self
            .agent
            .post(url.as_str())
            .set("Content-MD5", &md5)
            .set("Content-Type", "application/xml");
        let response = send(request, Some(Body::Text(body)))?;
        let text = read_text(response, MAX_XML_BODY)?;
        let document = roxmltree::Document::parse(&text).map_err(|_| CallError::Other)?;
        Ok(document
            .root_element()
            .children()
            .any(|node| node.is_element() && node.tag_name().name() == "Error"))
    }
}

enum Body {
    File(File),
    Text(String),
}

/// Send a request; a status of 300 or more is an error response (botocore's `ClientError`).
fn send(request: ureq::Request, body: Option<Body>) -> Result<ureq::Response, CallError> {
    let result = match body {
        None => request.call(),
        Some(Body::File(file)) => request.send(file),
        Some(Body::Text(text)) => request.send_string(&text),
    };
    match result {
        Ok(response) if response.status() < 300 => Ok(response),
        Ok(response) | Err(ureq::Error::Status(_, response)) => Err(status_error(response)),
        Err(ureq::Error::Transport(transport)) => Err(transport_error(&transport)),
    }
}

fn status_error(response: ureq::Response) -> CallError {
    let status = response.status();
    let code = read_text(response, MAX_ERROR_BODY)
        .ok()
        .and_then(|text| error_code(&text))
        .unwrap_or_else(|| status.to_string());
    CallError::Status { status, code }
}

/// The `<Code>` of an S3 error document.
fn error_code(text: &str) -> Option<String> {
    let document = roxmltree::Document::parse(text).ok()?;
    let root = document.root_element();
    let code = root
        .children()
        .find(|node| node.is_element() && node.tag_name().name() == "Code")?;
    Some(element_text(code)).filter(|code| !code.is_empty())
}

/// botocore's view of a transport failure: endpoint and connect errors, read timeouts, closed
/// connections, malformed responses and truncated bodies are retryable; an unusable URL, a TLS
/// failure or a proxy failure is not.
fn transport_error(transport: &ureq::Transport) -> CallError {
    use ureq::ErrorKind;
    let retryable = match transport.kind() {
        ErrorKind::Dns
        | ErrorKind::ConnectionFailed
        | ErrorKind::BadStatus
        | ErrorKind::BadHeader => true,
        ErrorKind::Io => std::error::Error::source(transport)
            .and_then(|source| source.downcast_ref::<io::Error>())
            .is_none_or(|error| io_retryable(error.kind())),
        _ => false,
    };
    CallError::Transport { retryable }
}

/// Whether an I/O failure on the connection is one botocore retries: timeouts, resets, an early
/// EOF and a malformed response (urllib3's `ProtocolError`, botocore's `ConnectionClosedError`)
/// are; a TLS failure (rustls reports it as `InvalidData`; botocore's `SSLError`) is not.
fn io_retryable(kind: io::ErrorKind) -> bool {
    kind != io::ErrorKind::InvalidData
}

fn content_length(response: &ureq::Response) -> Option<u64> {
    response.header("Content-Length")?.trim().parse().ok()
}

/// Stream a response body into `writer` in 1 MiB chunks; more than `cap` bytes is
/// `storage-byte-budget` (not retryable, as raised inside the transfer).
fn stream_body(mut reader: impl Read, writer: &mut File, cap: u64) -> Result<u64, CallError> {
    let mut buffer = vec![0u8; 1024 * 1024];
    let mut used: u64 = 0;
    loop {
        let count = match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => count,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => {
                return Err(CallError::Transport {
                    retryable: io_retryable(error.kind()),
                });
            }
        };
        used += u64::try_from(count).expect("a chunk fits u64");
        if used > cap {
            return Err(CallError::Storage(StorageError::new("storage-byte-budget")));
        }
        writer
            .write_all(&buffer[..count])
            .map_err(|_| CallError::Other)?;
    }
    writer.flush().map_err(|_| CallError::Other)?;
    Ok(used)
}

fn read_text(response: ureq::Response, limit: u64) -> Result<String, CallError> {
    let mut text = String::new();
    response
        .into_reader()
        .take(limit)
        .read_to_string(&mut text)
        .map_err(|error| CallError::Transport {
            retryable: io_retryable(error.kind()),
        })?;
    Ok(text)
}

/// The text of an element (its text children), trimmed.
fn element_text(node: roxmltree::Node<'_, '_>) -> String {
    let text: String = node
        .children()
        .filter(roxmltree::Node::is_text)
        .filter_map(|child| child.text())
        .collect();
    text.trim().to_string()
}

/// A `ListBucketResult`: keys (URL-decoded when the response says `EncodingType=url`, as
/// botocore decodes them), `IsTruncated` and `NextContinuationToken`.
fn parse_list(text: &str) -> Option<ListPage> {
    let document = roxmltree::Document::parse(text).ok()?;
    let root = document.root_element();
    if root.tag_name().name() != "ListBucketResult" {
        return None;
    }
    let child = |name: &str| {
        root.children()
            .find(|node| node.is_element() && node.tag_name().name() == name)
            .map(element_text)
    };
    let url_encoded = child("EncodingType").as_deref() == Some("url");
    let mut keys = Vec::new();
    for contents in root
        .children()
        .filter(|node| node.is_element() && node.tag_name().name() == "Contents")
    {
        let key = contents
            .children()
            .find(|node| node.is_element() && node.tag_name().name() == "Key")?;
        let raw: String = key.children().filter_map(|node| node.text()).collect();
        keys.push(if url_encoded { unquote_plus(&raw) } else { raw });
    }
    Some(ListPage {
        keys,
        truncated: child("IsTruncated").as_deref() == Some("true"),
        next_token: child("NextContinuationToken"),
    })
}

/// A bucket name usable as a virtual host over HTTPS (DNS-compatible, no dots).
fn virtual_host_compatible(bucket: &str) -> bool {
    (3..=63).contains(&bucket.len())
        && bucket
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && !bucket.starts_with('-')
        && !bucket.ends_with('-')
}

/// `AWS_ENDPOINT_URL_S3`, then `AWS_ENDPOINT_URL` (botocore's configured endpoints), unless
/// `AWS_IGNORE_CONFIGURED_ENDPOINT_URLS` is `true`.
fn configured_endpoint() -> Option<String> {
    let ignored = std::env::var("AWS_IGNORE_CONFIGURED_ENDPOINT_URLS")
        .is_ok_and(|value| value.eq_ignore_ascii_case("true"));
    if ignored {
        return None;
    }
    ["AWS_ENDPOINT_URL_S3", "AWS_ENDPOINT_URL"]
        .iter()
        .find_map(|name| std::env::var(name).ok().filter(|value| !value.is_empty()))
}

/// The credential configuration is unusable: a named profile that exists nowhere, or half a key
/// pair in the environment (botocore fails creating the client).
struct UnusableCredentials;

/// The credentials botocore's chain would find in the environment or the profile files;
/// `Ok(None)` when there are none (each request then fails, not retryable).
fn credentials() -> Result<Option<Credentials>, UnusableCredentials> {
    let variable = |name: &str| std::env::var(name).ok().filter(|value| !value.is_empty());
    if let Some(key) = variable("AWS_ACCESS_KEY_ID") {
        let secret = variable("AWS_SECRET_ACCESS_KEY").ok_or(UnusableCredentials)?;
        let token = variable("AWS_SESSION_TOKEN").or_else(|| variable("AWS_SECURITY_TOKEN"));
        return Ok(Some(match token {
            Some(token) => Credentials::new_with_token(key, secret, token),
            None => Credentials::new(key, secret),
        }));
    }
    let named = variable("AWS_PROFILE").or_else(|| variable("AWS_DEFAULT_PROFILE"));
    let profile = named.clone().unwrap_or_else(|| "default".to_string());
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    let shared = variable("AWS_SHARED_CREDENTIALS_FILE")
        .map_or_else(|| home.join(".aws/credentials"), PathBuf::from);
    let config =
        variable("AWS_CONFIG_FILE").map_or_else(|| home.join(".aws/config"), PathBuf::from);
    let config_section = if profile == "default" {
        "default".to_string()
    } else {
        format!("profile {profile}")
    };
    let sections = [
        ini_section(&shared, &profile),
        ini_section(&config, &config_section),
    ];
    if named.is_some() && sections.iter().all(Option::is_none) {
        return Err(UnusableCredentials);
    }
    for section in sections.into_iter().flatten() {
        let value = |name: &str| {
            section
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, v)| v.clone())
        };
        if let (Some(key), Some(secret)) =
            (value("aws_access_key_id"), value("aws_secret_access_key"))
        {
            return Ok(Some(match value("aws_session_token") {
                Some(token) => Credentials::new_with_token(key, secret, token),
                None => Credentials::new(key, secret),
            }));
        }
    }
    Ok(None)
}

/// The `key = value` pairs of one `[section]` of an INI profile file (keys lower-cased, values
/// trimmed, `#` / `;` comment lines skipped); `None` when the file or the section is absent.
fn ini_section(path: &Path, wanted: &str) -> Option<Vec<(String, String)>> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut current: Option<String> = None;
    let mut found = false;
    let mut pairs = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with(';') {
            continue;
        }
        if let Some(name) = trimmed
            .strip_prefix('[')
            .and_then(|rest| rest.strip_suffix(']'))
        {
            current = Some(name.trim().to_string());
            found |= name.trim() == wanted;
            continue;
        }
        if current.as_deref() != Some(wanted) || line.starts_with([' ', '\t']) {
            continue;
        }
        if let Some(index) = trimmed.find(['=', ':']) {
            let key = trimmed[..index].trim().to_lowercase();
            let value = trimmed[index + 1..].trim().to_string();
            pairs.push((key, value));
        }
    }
    found.then_some(pairs)
}
