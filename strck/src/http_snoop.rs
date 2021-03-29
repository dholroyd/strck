//! Wrapper around a limited subset of the API of the `reqwest` crate that takes a record of all
//! HTTP responses, for diagnostic purposes.

use reqwest::IntoUrl;
use std::borrow::Borrow;
use futures::prelude::*;
use encoding_rs::{Encoding, UTF_8};
use std::borrow::Cow;
use mime::Mime;
use bytes::Bytes;
use hyper;
use reqwest::Url;
use hyper::http::HeaderValue;
use reqwest::header::{AsHeaderName, HeaderName};
use std::convert::TryFrom;
use hyper::{http, StatusCode};
use log::info;
use std::{time, fmt};
use std::net::SocketAddr;
use std::str::FromStr;
use serde::Serializer;
use serde::export::fmt::Debug;
use serde::export::Formatter;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::UNIX_EPOCH;

#[derive(Debug, PartialEq)]
pub enum ExtraHeaderError {
    MissingColon,
    InvalidName,
    InvalidValue,
}
impl ToString for ExtraHeaderError {
    fn to_string(&self) -> String {
        match self {
            ExtraHeaderError::MissingColon => "Header definition should contain a colon \":\"",
            ExtraHeaderError::InvalidName => "Invalid header name",
            ExtraHeaderError::InvalidValue => "Invalid header value",
        }.to_owned()
    }
}
#[derive(Debug, PartialEq)]
pub struct ExtraHeader {
    pub name: reqwest::header::HeaderName,
    pub value: reqwest::header::HeaderValue,
}
impl FromStr for ExtraHeader {
    type Err = ExtraHeaderError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut itr = s.splitn(2, ":");
        let name = itr
            .next().unwrap();
        let value = itr
            .next()
            .ok_or(ExtraHeaderError::MissingColon)?;

        let name = name
            .trim()
            .parse()
            .map_err(|_e| ExtraHeaderError::InvalidName)?;

        let value = value
            .trim()
            .parse()
            .map_err(|_e| ExtraHeaderError::InvalidValue)?;

        Ok(ExtraHeader {
            name,
            value,
        })
    }
}

#[derive(Clone)]
pub struct HttpRef {
    inner: std::rc::Rc<HttpInfo>,
}
impl HttpRef {
    pub fn id(&self) -> uuid::Uuid {
        self.inner.id
    }
    pub fn info(&self) -> &HttpInfo {
        self.inner.as_ref()
    }
}
impl serde::Serialize for HttpRef {
    fn serialize<S>(&self, serializer: S) -> Result<<S as Serializer>::Ok, <S as Serializer>::Error> where
        S: Serializer
    {
        let time = self.inner.time.duration_since(UNIX_EPOCH).unwrap().as_millis() as u64;
        serializer.serialize_str(&format!("{}/{}", time, blob_uuid::to_blob(&self.id())))
    }
}
impl Debug for HttpRef {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_tuple("HttpRef")
            .field(&blob_uuid::to_blob(&self.id()))
            .finish()
    }
}

pub struct HttpResponseInfo {
    pub status: hyper::StatusCode,
    pub headers: hyper::HeaderMap,
    pub version: hyper::Version,
    pub body: Result<BodyInfo, BodyError>,
    pub remote_address: Option<SocketAddr>,
}
impl HttpResponseInfo {
    pub fn hash(&self) -> Result<u64, &BodyError> {
        self.body.as_ref().map(|b| b.hash )
    }
}

pub struct BodyInfo {
    // the actual body payload bytes
    pub data: bytes::Bytes,
    // fingerprint calculated from the payload bytes so as to enable cheap tests for equality
    // between one response payload and another
    pub hash: u64,
}

pub static HTTP_INFO_LIVE_COUNT: AtomicUsize = AtomicUsize::new(0);

pub struct HttpInfo {
    pub id: uuid::Uuid,
    pub url: Url,
    pub time: std::time::SystemTime,
    pub time_total: std::time::Duration,
    pub time_pretransfer: Option<std::time::Duration>,
    pub response: Result<HttpResponseInfo, reqwest::Error>,
    pub content_role: Option<String>,
}
impl HttpInfo {
    pub fn new(
        id: uuid::Uuid,
        url: Url,
        time: std::time::SystemTime,
        time_total: std::time::Duration,
        time_pretransfer: Option<std::time::Duration>,
        response: Result<HttpResponseInfo, reqwest::Error>,
        content_role: Option<String>
    ) -> HttpInfo {
        HTTP_INFO_LIVE_COUNT.fetch_add(1, Ordering::SeqCst);
        HttpInfo {
            id,
            url,
            time,
            time_total,
            time_pretransfer,
            response,
            content_role,
        }
    }
}
impl Drop for HttpInfo {
    fn drop(&mut self) {
        HTTP_INFO_LIVE_COUNT.fetch_sub(1, Ordering::SeqCst);
    }
}

pub trait Snoop: Clone {
    fn snoop(&mut self, event: HttpRef);
    fn close(self);
}

#[derive(Clone)]
pub struct Client<S: Snoop> {
    client: reqwest::Client,
    response_limit_bytes: usize,
    snoop: S,
    request_count: std::rc::Rc<std::cell::Cell<u64>>,
    max_requests: Option<u64>,
}

impl<S: Snoop> Client<S> {
    pub fn new(client: reqwest::Client, max_requests: Option<u64>, response_limit_bytes: usize, snoop: S) -> Client<S> {
        Client {
            client,
            response_limit_bytes,
            snoop,
            request_count: std::rc::Rc::new(std::cell::Cell::new(0)),
            max_requests,
        }
    }

    pub fn get<U: IntoUrl>(&self, url: U) -> RequestBuilder<S> {
        RequestBuilder::new(self.clone(), self.client.get(url))
    }

    pub async fn close(self) {
        info!("attempted {} HTTP requests", self.request_count.get());
        self.snoop.close()
    }

    pub fn total_request_count(&self) -> u64 {
        self.request_count.get()
    }
}

#[derive(Debug)]
pub enum BodyError {
    Http(reqwest::Error),
    ResponseSize(usize),
}

pub struct RequestBuilder<S: Snoop> {
    id: uuid::Uuid,
    client: Client<S>,
    builder: Option<reqwest::RequestBuilder>,
    content_role: Option<String>,
}

impl<S: Snoop> RequestBuilder<S> {
    fn new(client: Client<S>, builder: reqwest::RequestBuilder) -> RequestBuilder<S> {
        RequestBuilder {
            id: uuid::Uuid::new_v4(),
            client,
            builder: Some(builder),
            content_role: None,
        }
    }

    pub fn header<K, V>(&mut self, key: K, value: V)
        where
            HeaderName: TryFrom<K>,
            <HeaderName as TryFrom<K>>::Error: Into<http::Error>,
            HeaderValue: TryFrom<V>,
            <HeaderValue as TryFrom<V>>::Error: Into<http::Error>,
    {
        self.builder = Some(self.builder.take().unwrap().header(key, value));
    }

    /// Label optionally noting the client's expectation of the content being requested.
    /// This information is not sent in the request, but may be logged to help understand why
    /// a particular request was issued (this context might be useful if neither the URL requested
    /// nor the content type of the response unambiguously provide this information).
    pub fn content_role(&mut self, role: &str) {
        self.content_role = Some(role.to_owned());
    }

    pub async fn send(mut self) -> Result<Response, Error> {
        let c = self.client.request_count.get();
        if let Some(max) = self.client.max_requests {
            if c >= max {
                return Err(Error::NumberOfRequestsExceedsLimit(max))
            }
        }
        self.client.request_count.replace(c + 1);
        let time = std::time::SystemTime::now();
        let time_start = std::time::Instant::now();
        let mut builder = self.builder.take().unwrap();
        builder = self.add_request_id(builder);
        let req = builder.build().expect("RequestBuilder::build() failed unexpectedly");
        let url = req.url().clone();
        let id = self.id;
        //println!("{:?}", req.headers());
        let content_role = self.content_role.clone();
        let mut client = self.client.clone();
        let resp = self.client.client.execute(req)
            .await
            .map(|resp| async {
                let time_body_start = std::time::Instant::now();
                let limit = self.client.response_limit_bytes;
                let status = resp.status();
                let headers = resp.headers().clone();
                let version = resp.version();
                let remote_address = resp.remote_addr();
                let buffer = if let Some(content_length) = resp.content_length() {
                    if content_length as usize <= limit as usize {
                        Vec::with_capacity(content_length as usize)
                    } else {
                        vec![]
                    }
                } else {
                    vec![]
                };
                let body = resp.bytes_stream()
                    .map_err(BodyError::Http)
                    .try_fold(buffer, |mut v, c| async move {
                        if v.len() + c.len() <= limit {
                            v.extend_from_slice(c.borrow());
                            Ok(v)
                        } else {
                            Err(BodyError::ResponseSize(limit))
                        }
                    }).map_ok(bytes::Bytes::from).await;
                const RANDOM_SEED: u64 = 0x3C089B1F88804C3F;
                let body = body.map(|data| {
                    let hash = wyhash::wyhash(data.as_ref(), RANDOM_SEED);
                    BodyInfo { data, hash }
                });
                let info = HttpResponseInfo {
                    status,
                    headers: headers.clone(),
                    version,
                    body,
                    remote_address,
                };
                (info, time_body_start)
            });
        let resp = match resp {
            Ok(r) => Ok(r.await),
            Err(e) => Err(e),
        };
        let time_body_end = std::time::Instant::now();
        let time_body_start = match resp {
            Ok((ref _s, t)) => Some(t),
            Err(ref _e) => None,
        };
        let status_desc = match resp {
            Ok((ref s, _t)) => s.status.as_str().to_owned(),
            Err(ref e) => format!("{:?}", e),
        };
        //info!("{} {} {:>8} {}", blob_uuid::to_blob(&id), status_desc, time_body_end.duration_since(time_start).as_millis(), url);
        let info = HttpInfo::new(
            id,
            url,
            time,
            time_body_end.duration_since(time_start),
            time_body_start.map(|t| t.duration_since(time_start) ),
            resp.map(|(r, _t)| r ),
            content_role,
        );
        let href = HttpRef { inner: std::rc::Rc::new(info) };
        client.snoop.snoop(href.clone());
        if let Err(e) = &href.inner.response {
            Err(Error::from(href))
        } else {
            Ok(Response {
                inner: href,
            })
        }
    }

    /// tag the request being sent with our per-request uuid, for potential log correlation
    fn add_request_id(&self, builder: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        builder.header("X-Request-Id", blob_uuid::to_blob(&self.id))
    }

    pub fn req_id(&self) -> uuid::Uuid {
        self.id
    }
}

pub struct Response {
    inner: HttpRef,
}
impl Response {
    pub fn error_for_status_ref(&self) -> Result<&Self, Error> {
        let stat = self
            .inner
            .info()
            .response
            .as_ref()
            .unwrap()
            .status;
        if stat.is_client_error() || stat.is_server_error() {
            Err(Error::Status(self.inner.clone()))
        } else {
            Ok(self)
        }
    }
    pub fn status(&self) -> StatusCode {
        self
            .inner
            .info()
            .response
            .as_ref()
            .unwrap()
            .status
    }
    pub async fn text(&self) -> Result<String, Error> {
        self.text_with_charset("utf-8").await
    }
    pub async fn bytes(&self) -> Result<&Bytes, &BodyError> {
        self
            .inner
            .info()
            .response
            .as_ref()
            .unwrap()
            .body
            .as_ref()
            .map(|v| &v.data )
            .map_err(|e| e.clone() )
    }
    pub async fn text_with_charset(&self, default_encoding: &str) -> Result<String, Error> {
        // mostly copied from reqwest
        let content_type = self.inner
            .info()
            .response
            .as_ref()
            .unwrap()
            .headers
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<Mime>().ok());
        let encoding_name = content_type
            .as_ref()
            .and_then(|mime| mime.get_param("charset").map(|charset| charset.as_str()))
            .unwrap_or(default_encoding);
        let encoding = Encoding::for_label(encoding_name.as_bytes()).unwrap_or(UTF_8);

        let full = self.bytes().await.map_err(|_e| Error::from(self.inner.clone()))?;

        let (text, _, _) = encoding.decode(&full);
        if let Cow::Owned(s) = text {
            return Ok(s);
        }
        String::from_utf8(full.to_vec()).map_err(|_e| Error::RequestDecode(self.inner.clone()))
    }

    pub fn req_id(&self) -> uuid::Uuid {
        self.inner.id()
    }

    pub fn total_time(&self) -> time::Duration {
        self.inner.info().time_total
    }

    pub fn header<K: AsHeaderName>(&self, key: K) -> Option<&HeaderValue>{
        self
            .inner
            .info()
            .response
            .as_ref()
            .unwrap()
            .headers
            .get(key)
    }

    pub fn headers(&self) -> &hyper::HeaderMap {
        &self
            .inner
            .info()
            .response
            .as_ref()
            .unwrap()
            .headers
    }

    pub fn href(&self) -> HttpRef {
        self.inner.clone()
    }
}

#[derive(Debug, Clone)]
pub enum Error {
    RequestTimeout(HttpRef),
    RequestRedirect(HttpRef),
    RequestDecode(HttpRef),
    RequestBody(HttpRef),
    /// the reqwests crate reported an error with the request, but we aren't given specific detail
    RequestUnknownFault(HttpRef),
    Status(HttpRef),
    ResponseSizeExceedsLimit(HttpRef, usize),
    NumberOfRequestsExceedsLimit(u64),
    //ChannelSend(futures::channel::mpsc::SendError),
}
//impl From<futures::channel::mpsc::SendError> for Error {
//    fn from(e: futures::channel::mpsc::SendError) -> Self {
//        Error::ChannelSend(e)
//    }
//}
impl Error {
    fn from(req: HttpRef) -> Self {
        let e = req
            .inner
            .response
            .as_ref()
            .err()
            .expect("http_snoop::Error::from() called with non-error response reference");
        if e.is_timeout() {
            Error::RequestTimeout(req)
        } else if e.is_redirect() {
            Error::RequestRedirect(req)
        } else if e.is_decode() {
            Error::RequestDecode(req)
        } else if e.is_body() {
            Error::RequestBody(req)
        } else if e.is_request() {
            Error::RequestUnknownFault(req)
        } else {
            panic!("reqwest: {:?}", e)
        }
    }
}

#[cfg(test)]
mod test {
    use std::str::FromStr;
    use crate::http_snoop::{ExtraHeader, ExtraHeaderError};

    #[test]
    fn empty_header() {
        assert_eq!(ExtraHeader::from_str(""), Err(ExtraHeaderError::MissingColon));
    }
    #[test]
    fn only_colon() {
        assert_eq!(ExtraHeader::from_str(":"), Err(ExtraHeaderError::InvalidName));
    }
    #[test]
    fn simple_header() {
        assert_eq!(ExtraHeader::from_str("a: b"), Ok(ExtraHeader { name: reqwest::header::HeaderName::from_static("a"), value: reqwest::header::HeaderValue::from_static("b") }));
    }
}
