use crate::http_snoop;
use hls_m3u8::tags::VariantStream;
use crate::event_log::EventSink;
use futures::prelude::*;
use serde_derive::Serialize;
use hls_m3u8::types::PlaylistType;
use std::time;
use reqwest::header;
use hyper::StatusCode;
use crate::metric::Metric;
use crate::http_snoop::{Snoop, HttpRef, Error};
use std::convert::TryFrom;
use hls_m3u8::parser::ParseError;

// TODO: support VOD + Event manifests as well as Live

pub mod check;
mod timeline;

#[derive(Serialize)]
pub struct ManifestRef {
    req_id: HttpRef,
    line: Option<usize>,  // ambition; not reality
}

/// two media manifests between which something of note changed
#[derive(Serialize)]
pub struct Delta {
    before: ManifestRef,
    after: ManifestRef,
}


#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
#[serde(tag = "event_name")]
pub enum HlsEvent {
    LoadedMain {
        req_id: HttpRef,
        variant_count: usize,
    },
    MsnGoneBackwards {
        delta: Delta,
        last_msn: usize,
        this_msn: usize,
    },
    /// The earlier playlist had #EXT-X-ENDLIST, but in the current playlist its gone again
    EndListTagRemoved,
    UnexpectedPlaylistPropertyAddition {
        delta: Delta,
        name: &'static str
    },
    UnexpectedPlaylistPropertyRemoval {
        delta: Delta,
        name: &'static str
    },
    TargetDurationChanged {
        delta: Delta,
        last_target_duration_millis: u64,
        this_target_duration_millis: u64,
    },
    PlaylistTypeChanged {
        delta: Delta,
        #[serde(serialize_with="ser_playlist_type")]
        last_type: Option<PlaylistType>,
        #[serde(serialize_with="ser_playlist_type")]
        this_type: Option<PlaylistType>,
    },
    ManifestHistoryChangedUri {
        delta: Delta,
        msn: usize,
        last_uri: String,
        this_uri: String,
    },
    ManifestHistoryAddedDiscontinuity {
        delta: Delta,
        msn: usize,
    },
    ManifestHistoryRemovedDiscontinuity {
        delta: Delta,
        msn: usize,
    },
    ManifestHistoryChangedSegmentDuration {
        delta: Delta,
        msn: usize,
        last_duration_millis: u64,
        this_duration_millis: u64,
    },
    ManifestHistoryChangedSegmentByterange {
        delta: Delta,
        msn: usize,
        last_byterange: Option<String>,
        this_byterange: Option<String>,
    },
    /// One or more segments that used to be and the end of the playlist have disappeared
    LiveSegmentsRemoved {
        delta: Delta,
        last_msn: usize,
        this_msn: usize,
        removed_count: usize,
    },
    /// We've performed multiple manifest reloads without receiving an update.
    ManifestStale {
        delta: Delta,
        since_list_update: usize,
    },
    /// The media manifest contains an `EXT-X-ENDLIST` tag, meaning that no more segments will be
    /// appended
    End {
        req_id: HttpRef,
    },
    SlowMediaManifestResponse {
        req_id: HttpRef,
        response_time_millis: u64,
        target_duration_millis: u64,
    },
    /// The value from the HTTP `Age` response header indicates that an HTTP cache has held this
    /// playlist longer than the Playlist's target target duration
    CachedTooLong {
        req_id: HttpRef,
        age: u64,
        target_duration: u64,
    },
    IncorrectContentType {
        req_id: HttpRef,
        content_type: Option<String>,
    },
    ContentTypeChanged {
        delta: Delta,
        last_content_type: Option<String>,
        this_content_type: Option<String>,
    },
    LastModifiedInFuture {
        req_id: HttpRef,
        date: String,
        last_modified: String
    },
    HttpErrorStatus {
        req_id: HttpRef,
        status_code: u16
    },
    // TODO: remove the need for this item by knowing specifically what failed at the HTTP layer
    HttpUnknownError {
        req_id: HttpRef,
    },
    HttpBodyError {
        req_id: HttpRef,
    },
    HttpDecodeError {
        req_id: HttpRef,
    },
    HttpRedirectError {
        req_id: HttpRef,
    },
    HttpTimeout {
        req_id: HttpRef,
    },
    PlaylistUtf8Error {
        req_id: HttpRef,
    },
    PlaylistParseError {
        req_id: HttpRef,
    },
    PlaylistMalformedUrl {
        req_id: HttpRef,
    },
    ResponseSizeExceedsLimit {
        req_id: HttpRef,
        limit: usize,
    },
    NumberOfRequestsExceedsLimit {
        limit: u64,
    },
    MediaPlaylistWithoutExtinf {
        req_id: HttpRef,
        url: String,
        start: usize,
        end: usize,
    },
    LastModifiedChangedButBodiesIdentical {
        delta: Delta,
        this_last_modified: String,
        last_last_modified: String,
    },
    DaterangeAttributeChanged {
        req_id: HttpRef,
        daterange_id: String,
        attr_name: String,
        prev_value: String,
        this_value: String,
    }
}
fn ser_playlist_type<S>(ty: &Option<PlaylistType>, s: S) -> Result<S::Ok, S::Error> where S: serde::Serializer {
    match ty {
        None => s.serialize_none(),
        Some(PlaylistType::Event) => s.serialize_str("event"),
        Some(PlaylistType::Vod) => s.serialize_str("vod"),
    }
}
impl From<HlsManifestError> for HlsEvent {
    fn from(e: HlsManifestError) -> Self {
        // TODO: many of these conversions loose relevant diagnostic information - keep more
        match e {
            HlsManifestError::HttpTimeout(req) => HlsEvent::HttpTimeout {
                req_id: req,
            },
            HlsManifestError::HttpStatus(req_id, status_code) => HlsEvent::HttpErrorStatus {
                req_id,
                status_code: status_code.as_u16(),
            },
            HlsManifestError::HttpUnknownFailure(req_id) => HlsEvent::HttpUnknownError {
                req_id,
            },
            HlsManifestError::HttpBody(href) => HlsEvent::HttpBodyError {
                req_id: href,
            },
            HlsManifestError::HttpDecode(req_id) => HlsEvent::HttpDecodeError {
                req_id,
            },
            HlsManifestError::HttpRedirect(req_id) => HlsEvent::HttpRedirectError {
                req_id,
            },
            HlsManifestError::Utf8(req_id, err) => HlsEvent::PlaylistUtf8Error {
                req_id,
            },
            HlsManifestError::Parse(req_id, err) => HlsEvent::PlaylistParseError {
                req_id,
            },
            HlsManifestError::Parse2(req_id, err) => HlsEvent::PlaylistParseError {
                req_id,
            },
            HlsManifestError::Url(req_id, err) => HlsEvent::PlaylistMalformedUrl {
                req_id,
            },
            HlsManifestError::ResponseSizeExceedsLimit(req_id, limit) => HlsEvent::ResponseSizeExceedsLimit{
                req_id,
                limit,
            },
            HlsManifestError::NumberOfRequestsExceedsLimit(limit) => HlsEvent::NumberOfRequestsExceedsLimit{
                limit,
            },
            HlsManifestError::NotModified => {
                // TODO: remove this variant of HlsManifestError
                panic!("Unexpected conversion {:?}", e);
            }
        }
    }
}

#[derive(Debug)]
pub enum HlsManifestError {
    HttpTimeout(HttpRef),
    HttpStatus(HttpRef, reqwest::StatusCode),
    HttpBody(HttpRef),
    HttpDecode(HttpRef),
    HttpRedirect(HttpRef),
    // maybe by bypassing reqwest and using hyper directly we can know the real problem in these cases
    HttpUnknownFailure(HttpRef),
    Utf8(HttpRef, std::string::FromUtf8Error),
    Parse(HttpRef, hls_m3u8::parser::ParseError),
    Parse2(HttpRef, hls_m3u8::Error),
    Url(HttpRef, url::ParseError),
    ResponseSizeExceedsLimit(HttpRef, usize),
    NumberOfRequestsExceedsLimit(u64),
    /// TODO: this should not be in the error-enum
    NotModified,
}
impl HlsManifestError {
    fn from_req(req: HttpRef) -> Self {
        let e = req.info().response.as_ref().err().expect("HlsManifestError::from_req() called with non-error http reference");
        if e.is_timeout() {
            HlsManifestError::HttpTimeout(req)
        } else if e.is_status() {
            HlsManifestError::HttpStatus(req.clone(), e.status().unwrap())
        } else if e.is_body() {
            HlsManifestError::HttpBody(req)
        } else if e.is_decode() {
            HlsManifestError::HttpDecode(req)
        } else if e.is_redirect() {
            HlsManifestError::HttpRedirect(req)
        } else {
            panic!("unexpected {:?}", e);
        }
    }
    fn from_err(e: http_snoop::Error) -> Self {
        match e {
            Error::RequestTimeout(href) => Self::HttpTimeout(href),
            Error::RequestRedirect(href) => Self::HttpRedirect(href),
            Error::RequestDecode(href) => Self::HttpDecode(href),
            Error::RequestBody(href) => Self::HttpBody(href),
            Error::Status(href) => HlsManifestError::HttpStatus(href.clone(), href.info().response.as_ref().unwrap().status),
            Error::ResponseSizeExceedsLimit(href, limit) => Self::ResponseSizeExceedsLimit(href, limit),
            Error::NumberOfRequestsExceedsLimit(limit) => Self::NumberOfRequestsExceedsLimit(limit),
            Error::RequestUnknownFault(href) => Self::HttpUnknownFailure(href)
        }
    }
}

pub struct HlsProcessor<S: Snoop, L: EventSink<Extra = HlsEvent>, M: Metric> {
    client: http_snoop::Client<S>,
    url: reqwest::Url,
    log: L,
    manifest_latency: M,
    stream_latency: M,
    msn_regression: M,
}
impl<S: Snoop, L: EventSink<Extra = HlsEvent>, M: Metric> HlsProcessor<S, L, M> {
    pub fn new(
        client: http_snoop::Client<S>,
        url: reqwest::Url,
        log: L,
        manifest_latency: M,
        stream_latency: M,
        msn_regression: M,
    ) -> HlsProcessor<S, L, M> {
        HlsProcessor {
            client,
            url,
            log,
            manifest_latency,
            stream_latency,
            msn_regression
        }
    }

    pub async fn start(mut self) -> Result<(), HlsManifestError> {
        let res = self.run().await;
        println!("HlsProcessor::run() -> {:?}", res);
        self.client.close().await;
        self.log.close();
        self.manifest_latency.close();
        res
    }
    async fn run(&mut self) -> Result<(), HlsManifestError> {
        let url = self.url.clone();
        let client = self.client.clone();
        let log = self.log.clone();
        // TODO: periodically reload the main manifest while live, and asset invariants
        let (href, body) = self.load_main_manifest().await?;
        let main_manifest = hls_m3u8::MasterPlaylist::try_from(body.as_str())
            .map_err(|e| HlsManifestError::Parse2(href.clone(), e) )?;
        let variant_urls: Result<Vec<_>, _> = main_manifest.variant_streams.iter().map(|var| {
            match var {
                VariantStream::ExtXIFrame { uri, .. } | VariantStream::ExtXStreamInf { uri, .. } => {
                    url.join(uri)
                },
            }
        }).collect();
        let mut variant_urls = variant_urls
            .map_err(|e| HlsManifestError::Url(href.clone(), e))?;
        self.log.info(HlsEvent::LoadedMain {
            req_id: href.clone(),
            variant_count: variant_urls.len(),
        });

        let media_urls: Result<Vec<_>, _> = main_manifest.media.iter().filter_map(|media| media.uri().map(|u| url.join(u)) ).collect();
        let media_urls = media_urls
            .map_err(|e| HlsManifestError::Url(href, e))?;
        variant_urls.extend(media_urls);

        // unlike a real HLS client, we process all media-manifests in parallel rather than
        // sticking with a single bitrate
        let items = futures::future::join_all(
            variant_urls.into_iter().map(move |u| {
                let mut log = log.clone();
                // TODO: ideally track separate stream_latency metrics per stream rather than aggregating
                process_media_manifest(client.clone(), self.manifest_latency.clone(), self.stream_latency.clone(), self.msn_regression.clone(), u, log.clone())
                    .map_err(move |res| log.error(res.into()) )
            })
        ).await;
        println!("items {:?}", items);
        Ok(())
    }

    async fn load_main_manifest(&self) -> Result<(HttpRef, String), HlsManifestError>{
        let mut req = self.client.get(self.url.clone());
        req.content_role("hls_main_manifest");
        let req_id = req.req_id();
        let resp = req.send().await.map_err(|e| HlsManifestError::from_err(e))?;
        if resp.status().is_client_error() || resp.status().is_server_error() {
            return Err(HlsManifestError::HttpStatus(resp.href(), resp.status()))
        }
        // TODO: enforce size limit on manifest response to avoid memory exhaustion
        resp.text().await
            .map(|r| (resp.href(), r))
            .map_err(|e| HlsManifestError::from_err(e) )
    }
}

struct MediaManifestState<S: Snoop> {
    client: http_snoop::Client<S>,
    url: reqwest::Url,
    delay: Option<time::Duration>,
    prev_last_msn: Option<usize>,
    prev_etag: Option<String>,
    prev_last_modified: Option<String>,
}
impl<S: Snoop> MediaManifestState<S> {
    fn new(client: http_snoop::Client<S>, url: reqwest::Url) -> MediaManifestState<S> {
        MediaManifestState {
            client,
            url,
            delay: None,
            prev_last_msn: None,
            prev_etag: None,
            prev_last_modified: None,
        }
    }
}

const MAX_SEQUENTIAL_PLAYLIST_LOAD_ERROR_COUNT: usize = 100;

async fn process_media_manifest<S, M, L>(
    client: http_snoop::Client<S>,
    mut manifest_latency: M,
    mut stream_latency: M,
    mut msn_regression: M,
    url: reqwest::Url,
    mut log: L
) -> Result<(), HlsManifestError>
    where
        S: Snoop,
        M: Metric,
        L: EventSink<Extra=HlsEvent>
{
    let mut chk = check::MediaPlaylistCheck::new(log.clone(), msn_regression);
    let mut manifest_state = MediaManifestState::new(client, url);
    let mut playlist_sequential_load_error_count = 0;
    loop {
        if let Some(delay) = manifest_state.delay {
            tokio::time::delay_for(delay).await;
        }
        // TODO: if loading the manifest errors, we should try to continue polling in many cases,
        //       while currently we just stop processing the particular representation
        match load_media_manifest(manifest_state.client.clone(), manifest_state.url.clone(), manifest_state.prev_etag.clone(), manifest_state.prev_last_modified.clone(), &mut log).await {
            Ok(response) => {
                playlist_sequential_load_error_count = 0;
                manifest_latency.put(response.total_time.as_millis() as u64);
                let target_duration = response.playlist.target_duration;
                let delay = if let (Some(prev_last_msn), Some(last_seg)) = (manifest_state.prev_last_msn, response.playlist.last_segment()) {
                    if last_seg.number() == prev_last_msn {
                        // "If the client reloads a Playlist file and finds that it has not
                        // changed, then it MUST wait for a period of one-half the Target
                        // Duration before retrying."
                        target_duration / 2
                    } else {
                        last_seg.duration().duration()
                    }
                } else {
                    target_duration
                };
                if let Some(end_program_datetime) = find_end_time(&response.playlist) {
                    let latency = chrono::Utc::now() - chrono::DateTime::<chrono::Utc>::from(end_program_datetime);
                    if latency >= chrono::Duration::seconds(0) {
                        // TODO: be able to track negative latency (probably indication of a bug)
                        stream_latency.put(latency.num_milliseconds() as u64)
                    }
                }
                let manifest_had_endlist = response.playlist.has_end_list;
                let playlist_type = response.playlist.playlist_type;
                manifest_state.prev_last_msn = response.playlist.last_segment().map(|s| s.number());
                manifest_state.delay = Some(delay);
                // remember any cache-validators to enable conditional reload of the manifest next time,
                manifest_state.prev_etag = response.href.info().response.as_ref().unwrap().headers
                    .get(header::ETAG)
                    .and_then(|v| v.to_str().ok() )
                    .map(ToOwned::to_owned);
                manifest_state.prev_last_modified = response.href.info().response.as_ref().unwrap().headers
                    .get(header::LAST_MODIFIED)
                    .and_then(|v| v.to_str().ok() )
                    .map(ToOwned::to_owned);
                chk.next_playlist(response.href, response.playlist, response.total_time);
                if manifest_had_endlist || playlist_type == Some(hls_m3u8::types::PlaylistType::Vod){
                    break;
                }
            },
            Err(HlsManifestError::NotModified) => {
                chk.not_modified();
                // TODO: next delay should be half duration not full duration
                continue;
            },
            Err(e) => {
                playlist_sequential_load_error_count += 1;
                match e {
                    HlsManifestError::HttpStatus(ref href, status) => {
                        chk.error_status(href.clone(), status);
                    },
                    HlsManifestError::HttpTimeout(ref href) => {
                        chk.timeout(href.clone());
                    },
                    _ => {
                        return Err(e)
                    }
                }
                if playlist_sequential_load_error_count > MAX_SEQUENTIAL_PLAYLIST_LOAD_ERROR_COUNT {
                    return Err(e)
                }
                if manifest_state.delay.is_none() {
                    // add a delay to prevent going into a tight-loop if the initial manifest load results in an error
                    manifest_state.delay = Some(time::Duration::from_secs(5))
                }
            }
        }
    }
    Ok(())
}
async fn load_media_manifest<S: Snoop, L: EventSink<Extra = HlsEvent>>(
    client: http_snoop::Client<S>,
    url: reqwest::Url,
    prev_etag: Option<String>,
    prev_last_modified: Option<String>,
    log: &mut L
) -> Result<MediaPlaylistResponse, HlsManifestError> {
    let mut req = client.get(url);
    req.content_role("hls_media_manifest");
    if let Some(prev_etag) = prev_etag {
        req.header(header::IF_NONE_MATCH, prev_etag);
    }
    if let Some(prev_last_modified) = prev_last_modified {
        req.header(header::IF_MODIFIED_SINCE, prev_last_modified);
    }
    let resp = req.send().await.map_err(|e| {
        HlsManifestError::from_err(e)
    })?;
    if resp.status().is_client_error() || resp.status().is_server_error() {
        return Err(HlsManifestError::HttpStatus(resp.href(), resp.status()))
    }
    if resp.status() == StatusCode::NOT_MODIFIED {
        return Err(HlsManifestError::NotModified);
    }
    let req_id = resp.req_id();

    let headers = resp.headers().clone();
    let total_time = resp.total_time();

    let body = resp.bytes().await.map_err(|e| HlsManifestError::HttpBody(resp.href()))?;

    let href = resp.href();

    hls_m3u8::parser::Parser::new(hls_m3u8::parser::Cursor::from(body.as_ref()))
        .parse()
        .and_then(|b| {
            for e in b.errors() {
                log.error(parse_err_to_event(href.clone(), e));
            }
            b.build()
        } )
        .map(|playlist| {
            MediaPlaylistResponse {
                href: resp.href(),
                playlist,
                total_time,
            }
        })
        .map_err(|e| HlsManifestError::Parse(resp.href(), e) )
}

fn parse_err_to_event(req: HttpRef, e: &hls_m3u8::parser::ParseError) -> HlsEvent {
    match e {
        ParseError::Incomplete { element_name, at } => unimplemented!(),
        ParseError::Unexpected { .. } => unimplemented!(),
        ParseError::Utf8(_, _) => unimplemented!(),
        ParseError::InvalidNumber => unimplemented!(),
        ParseError::ExpectedEndOfInput(_) => unimplemented!(),
        ParseError::Attributes => unimplemented!(),
        ParseError::Chrono(_) => unimplemented!(),
        ParseError::ParseFloatError { .. } => unimplemented!(),
        ParseError::MissingAttribute { .. } => unimplemented!(),
        ParseError::UnexpectedAttribute { .. } => unimplemented!(),
        ParseError::Hex { .. } => unimplemented!(),
        ParseError::MissingTargetDuration => unimplemented!(),
        ParseError::MissingVersion => unimplemented!(),
        ParseError::UrlWithoutExtinf { url, at } => HlsEvent::MediaPlaylistWithoutExtinf {
            req_id: req,
            url: url.to_owned(),
            start: at.start.0,
            end: at.end.0,
        },
        ParseError::PeekFailed => unreachable!("PeekFailed should never be visible outside of the parser"),  // TODO: remove PeekFailed from public interface
    }
}

struct MediaPlaylistResponse {
    href: HttpRef,
    playlist: hls_m3u8::parser::MyMediaPlaylist,
    total_time: time::Duration,
}

fn find_end_time(media_playlist: &hls_m3u8::parser::MyMediaPlaylist) -> Option<chrono::DateTime<chrono::FixedOffset>> {
    let mut datetime = None;
    // calculate forward from the last-seen EXT-X-PROGRAM-DATE-TIME through to the end of the
    // segment list, adding durations as we go,
    for seg in media_playlist.segments() {
        if let Some(ref prog_date_time) = seg.program_date_time() {
            datetime = Some(prog_date_time.date_time);
        }
        if let Some(t) = datetime.take() {
            datetime = Some(t + chrono::Duration::from_std(seg.duration().duration()).ok()?);
        }
    }
    datetime
}