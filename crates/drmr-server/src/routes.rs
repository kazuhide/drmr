//! HTTP dispatch for the common workflow and local static assets.
use crate::{
    App,
    http::{self, Body, Reply, Req},
    router::Endpoint,
    static_files,
};
use std::{path::PathBuf, sync::Arc};

#[derive(Debug)]
pub enum RouteError {
    NotFound,
    Http(u16),
    Internal,
}
impl RouteError {
    pub fn reply(self) -> Reply {
        match self {
            Self::NotFound => Reply::json_error(404, "not found"),
            Self::Http(status) => http::html_error(status),
            Self::Internal => http::html_error(500),
        }
    }
}
type Answer = Result<Reply, RouteError>;
pub fn reads_body(endpoint: Endpoint, method: &str) -> bool {
    endpoint == Endpoint::Common && matches!(method, "POST" | "DELETE")
}
pub fn dispatch(app: &Arc<App>, endpoint: Endpoint, values: &[String], req: &Req) -> Reply {
    let answer = match endpoint {
        Endpoint::Common => return app.common.dispatch(req),
        Endpoint::Index => page(app, req, "index.html"),
        Endpoint::Static => static_file(app, req, values.first().map_or("", String::as_str)),
    };
    answer.unwrap_or_else(RouteError::reply)
}

fn page(app: &Arc<App>, req: &Req, name: &str) -> Answer {
    let facts = static_files::page(&app.static_root, name).map_err(|error| match error.kind() {
        std::io::ErrorKind::NotFound => RouteError::NotFound,
        _ => RouteError::Internal,
    })?;
    if matches!(&facts.source, crate::http::FileSource::Disk(path) if app.common.denied(path)) {
        return Err(RouteError::NotFound);
    }
    let mimetype = static_files::guess_type(name).unwrap_or("application/octet-stream");
    Ok(http::send_file(req, &facts, mimetype))
}

fn static_file(app: &Arc<App>, req: &Req, filename: &str) -> Answer {
    let facts = static_files::lookup(&app.static_root, filename).ok_or(RouteError::Http(404))?;
    if matches!(&facts.source, crate::http::FileSource::Disk(path) if app.common.denied(path)) {
        return Err(RouteError::NotFound);
    }
    let name = facts
        .path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mimetype = static_files::guess_type(&name).unwrap_or("application/octet-stream");
    Ok(http::send_file(req, &facts, mimetype))
}

struct Sender {
    channel: tokio::sync::mpsc::Sender<Result<Vec<u8>, std::io::Error>>,
    workers: Arc<crate::workers::Workers>,
}

fn send_chunk(sender: &Sender, mut chunk: Result<Vec<u8>, std::io::Error>) -> bool {
    while sender.workers.is_open() {
        match sender.channel.try_send(chunk) {
            Ok(()) => return true,
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => return false,
            Err(tokio::sync::mpsc::error::TrySendError::Full(pending)) => {
                chunk = pending;
                if !sender.workers.wait(std::time::Duration::from_millis(5)) {
                    return false;
                }
            }
        }
    }
    false
}

/// Stream a bounded file slice on an owned, cancellable worker.
pub(crate) fn file_body(
    workers: &Arc<crate::workers::Workers>,
    path: PathBuf,
    start: u64,
    length: u64,
) -> Body {
    use std::io::{Read, Seek, SeekFrom};
    let (channel, receiver) = tokio::sync::mpsc::channel(8);
    let sender = Sender {
        channel,
        workers: workers.clone(),
    };
    let spawned = workers.spawn("file-body", move || {
        let mut file = match (|| {
            let mut file = std::fs::File::open(&path)?;
            file.seek(SeekFrom::Start(start))?;
            Ok(file)
        })() {
            Ok(file) => file,
            Err(error) => {
                let _ = send_chunk(&sender, Err(error));
                return;
            }
        };
        let mut remaining = length;
        let mut buffer = vec![0u8; 64 * 1024];
        while remaining > 0 {
            let want = usize::try_from(remaining.min(64 * 1024)).unwrap_or(64 * 1024);
            let result = file.read(&mut buffer[..want]);
            match result {
                Ok(0) => break,
                Ok(read) => {
                    remaining -= u64::try_from(read).unwrap_or(remaining);
                    if !send_chunk(&sender, Ok(buffer[..read].to_vec())) {
                        return;
                    }
                }
                Err(error) => {
                    let _ = send_chunk(&sender, Err(error));
                    return;
                }
            }
        }
    });
    drop(spawned);
    Body::Stream(receiver)
}
