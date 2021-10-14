use crate::hls::{HlsEvent, Delta, ManifestRef};
use crate::event_log::EventSink;
use super::timeline::*;
use std::time;
use hyper::http::HeaderValue;
use std::str::FromStr;
use crate::http_snoop::{HttpRef, HttpResponseInfo};
use crate::metric::Metric;
use reqwest::header;
use std::collections::HashMap;

// Set-of-u64 structure optimised for the case where multiple contiguous values are stored
struct SequenceSet {
    spans: Vec<SeqSpan>
}
struct SeqSpan {
    start: u64,
    count: u16,
}

struct PlaylistInfo {
    playlist: hls_m3u8::parser::MyMediaPlaylist,
    href: HttpRef,
}

enum LastError {
    None,
    HttpError(u16),
    Timeout,
}

pub struct MediaPlaylistCheck<L: EventSink<Extra = HlsEvent>, M: Metric> {
    log: L,
    last_playlist: Option<PlaylistInfo>,
    timeline: Timeline,
    since_last_update: usize,
    last_fresh_playlist_req: Option<HttpRef>,
    final_msn: Option<usize>,
    ended: bool,
    msn_regression: M,
    last_error: LastError,
}

fn delta(before: &PlaylistInfo, after: &PlaylistInfo) -> Delta {
    Delta {
        before: ManifestRef {
            req_id: before.href.clone(),
            line: None
        },
        after: ManifestRef {
            req_id: after.href.clone(),
            line: None
        }
    }
}

impl<L: EventSink<Extra = HlsEvent>, M: Metric> MediaPlaylistCheck<L, M> {
    pub fn new(log: L, msn_regression: M) -> MediaPlaylistCheck<L, M> {
        MediaPlaylistCheck {
            log,
            last_playlist: None,
            timeline: Timeline::default(),
            since_last_update: 0,
            last_fresh_playlist_req: None,
            final_msn: None,
            ended: false,
            msn_regression,
            last_error: LastError::None,
        }
    }

    pub fn not_modified(&mut self) {
        self.since_last_update += 1;
    }

    pub fn error_status(&mut self, href: HttpRef, status: reqwest::StatusCode) {
        let evt = HlsEvent::HttpErrorStatus {
            req_id: href,
            status_code: status.as_u16(),
        };
        if let LastError::HttpError(e) = self.last_error {
            if e == status.as_u16() {
                // only a warning if it's the same error last last time
                self.log.warning(evt)
            } else {
                self.log.error(evt)
            }
        } else {
            self.log.error(evt)
        }
        self.last_error = LastError::HttpError(status.as_u16())
    }

    pub fn timeout(&mut self, req: HttpRef) {
        let evt = HlsEvent::HttpTimeout {
            req_id: req,
        };
        if let LastError::Timeout = self.last_error {
            // only a warning if it's the same error last last time
            self.log.warning(evt);
        } else {
            self.log.error(evt);
        }
        self.last_error = LastError::Timeout;
    }

    pub fn next_playlist(&mut self, href: HttpRef, playlist: hls_m3u8::parser::MyMediaPlaylist, total_time: time::Duration) {
        self.last_error = LastError::None;
        let playlist_info = PlaylistInfo {
            href: href.clone(),
            playlist,
        };
        if let Some(last_playlist) = self.last_playlist.take() {
            self.check_invariant_properties(&last_playlist, &playlist_info);
            self.check_update(&last_playlist, &playlist_info);
        } else {
            self.check_initial_configuration(&playlist_info);
            self.timeline.append_new_segments(playlist_info.playlist.segments());
            // being the first copy of the playlist we've seen, it can't be stale,
            self.last_fresh_playlist_req = Some(playlist_info.href.clone());
        }
        self.check_headers(&playlist_info);
        // TODO: consider tuning the alert-level down.
        if total_time >= playlist_info.playlist.target_duration {
            self.log.error(HlsEvent::SlowMediaManifestResponse {
                req_id: href.clone(),
                response_time_millis: total_time.as_millis() as u64,
                target_duration_millis: playlist_info.playlist.target_duration.as_millis() as u64,
            })
        }
        if playlist_info.playlist.has_end_list && !self.ended {
            self.log.info(HlsEvent::End {
                req_id: href,
            });
            // remember that we've observed EXT-X-ENDLIST so that we don't emit HlsEvent::End again
            self.ended = true;
        }
        self.last_playlist = Some(playlist_info);
    }

    /// emit errors if things that are supposed to be fixed for the stream lifetime are actually
    /// seen to change during a live stream
    fn check_invariant_properties(&mut self, last: &PlaylistInfo, this: &PlaylistInfo) {
        if last.playlist.has_i_frames_only != this.playlist.has_i_frames_only {
            if last.playlist.has_i_frames_only {
                self.log.error(HlsEvent::UnexpectedPlaylistPropertyRemoval {
                    delta: delta(last, this),
                    name: "EXT-X-I-FRAMES-ONLY",
                })
            } else {
                self.log.error(HlsEvent::UnexpectedPlaylistPropertyAddition {
                    delta: delta(last, this),
                    name: "EXT-X-I-FRAMES-ONLY",
                })
            }
        }
        if last.playlist.has_independent_segments != this.playlist.has_independent_segments {
            if last.playlist.has_independent_segments {
                self.log.error(HlsEvent::UnexpectedPlaylistPropertyRemoval {
                    delta: delta(last, this),
                    name: "ext-x-independent-segments",
                })
            } else {
                self.log.error(HlsEvent::UnexpectedPlaylistPropertyAddition {
                    delta: delta(last, this),
                    name: "ext-x-independent-segments",
                })
            }
        }
        // TODO: check version (property not currently exposed)
        //if last.playlist.version != this.playlist.version {
        //
        //}
        if last.playlist.target_duration != this.playlist.target_duration {
            self.log.error(HlsEvent::TargetDurationChanged {
                delta: delta(last, this),
                last_target_duration_millis: last.playlist.target_duration.as_millis() as u64,
                this_target_duration_millis: this.playlist.target_duration.as_millis() as u64,
            })
        }
        let last_content_type = last.href.info().response.as_ref().unwrap().headers.get(reqwest::header::CONTENT_TYPE);
        let this_content_type = this.href.info().response.as_ref().unwrap().headers.get(reqwest::header::CONTENT_TYPE);
        if last_content_type != this_content_type {
            self.log.warning(HlsEvent::ContentTypeChanged {
                delta: delta(last, this),
                last_content_type: last_content_type
                    .and_then(|v| v.to_str().ok() )
                    .map(ToOwned::to_owned),
                this_content_type: this_content_type
                    .and_then(|v| v.to_str().ok() )
                    .map(ToOwned::to_owned),
            })
        }
        // TODO: this is not legitimate mid playback, however it's also not a problem we're seeing
        //       on normal usage, and is also guaranteed to happen at the env of every event when
        //       switching to 'pseudo-vod' mode.  Therefore suppressing for now to avoid false
        //       negative alerts,
        //if last.playlist.playlist_type != this.playlist.playlist_type {
        //    self.log.error(HlsEvent::PlaylistTypeChanged {
        //        last_type: last.playlist.playlist_type,
        //        this_type: this.playlist.playlist_type,
        //    })
        //}
    }

    fn check_update(&mut self, last: &PlaylistInfo, this: &PlaylistInfo) {
        // TODO: assert that the EXT-X-PROGRAM-DATE-TIME values continue to match up with the segments as items are removed from the top of the playlist etc

        // TODO: handle playlists that are empty, without panicking

        // Once the stream ends, it doesn't make sense for it to start again
        if last.playlist.has_end_list && !this.playlist.has_end_list {
            self.log.warning(HlsEvent::EndListTagRemoved)
        }
        // if the MSN changes, it should only ever increase
        if last.playlist.media_sequence > this.playlist.media_sequence {
            let regression = last.playlist.media_sequence - this.playlist.media_sequence;
            self.msn_regression.put(regression as u64);
            self.log.error(HlsEvent::MsnGoneBackwards {
                delta: delta(last, this),
                last_msn: last.playlist.media_sequence,
                this_msn: this.playlist.media_sequence,
            })
        } else {
            self.msn_regression.put(0);
            let last_msn = last.playlist.last_segment().unwrap().number();
            let this_msn = this.playlist.last_segment().unwrap().number();
            if last_msn > this_msn {
                let removed_count = last_msn - this_msn;
                let event = HlsEvent::LiveSegmentsRemoved {
                    delta: delta(&last, &this),
                    last_msn,
                    this_msn,
                    removed_count
                };
                if removed_count > 1 {
                    self.log.error(event);
                } else {
                    self.log.warning(event);
                }

            } else {
                // we can only perform these checks when the MSN values are sane,
                self.check_manifest_history_invariant(last, this);
                self.check_stale(this);
                self.update_timeline(last, this);
                self.check_daterange(last_msn, this);
            }
        }

        if let (Ok(this_resp), Ok(last_resp)) = (&this.href.info().response, &last.href.info().response) {
            self.check_last_modified_changed_but_bodies_identical(&last, &this, this_resp, last_resp);
            self.check_missed_not_modified_response(&last, &this, this_resp, last_resp);
        }
    }

    fn check_last_modified_changed_but_bodies_identical(&mut self, last: &&PlaylistInfo, this: &&PlaylistInfo, this_resp: &HttpResponseInfo, last_resp: &HttpResponseInfo) {
        if this_resp.hash().unwrap() == last_resp.hash().unwrap() {
            // response bodies are identical
            if let (Some(this_last_modified), Some(last_last_modified)) = (this_resp.headers.get(header::LAST_MODIFIED), last_resp.headers.get(header::LAST_MODIFIED)) {
                if this_last_modified != last_last_modified {
                    self.log.warning(HlsEvent::LastModifiedChangedButBodiesIdentical {
                        delta: delta(&last, &this),
                        this_last_modified: this_last_modified.to_str().unwrap().to_string(),
                        last_last_modified: last_last_modified.to_str().unwrap().to_string(),
                    })
                }
            }
        }
    }
    fn check_missed_not_modified_response(&mut self, last: &&PlaylistInfo, this: &&PlaylistInfo, this_resp: &HttpResponseInfo, last_resp: &HttpResponseInfo) {
        if this_resp.status != hyper::StatusCode::NOT_MODIFIED && Self::has_cache_validator(last_resp) {
            if this_resp.headers.get(header::ETAG) == last_resp.headers.get(header::ETAG) && this_resp.headers.get(header::LAST_MODIFIED) == last_resp.headers.get(header::LAST_MODIFIED) {
                self.log.warning(HlsEvent::MissedLastModifiedResponse {
                    delta: delta(&last, &this),
                    this_last_modified: this_resp.headers.get(header::LAST_MODIFIED).map(|v| v.to_str().ok() ).flatten().map(|s| s.to_string() ),
                    last_last_modified: last_resp.headers.get(header::LAST_MODIFIED).map(|v| v.to_str().ok() ).flatten().map(|s| s.to_string() ),
                    this_etag: this_resp.headers.get(header::ETAG).map(|v| v.to_str().ok() ).flatten().map(|s| s.to_string() ),
                    last_etag: last_resp.headers.get(header::ETAG).map(|v| v.to_str().ok() ).flatten().map(|s| s.to_string() ),
                })

            }
        }
    }

    fn has_cache_validator(resp: &HttpResponseInfo) -> bool {
        resp.headers.contains_key(header::ETAG) || resp.headers.contains_key(header::LAST_MODIFIED)
    }

    fn check_stale(&mut self, this: &PlaylistInfo) {
        let this_msn = this.playlist.last_segment().map(|s| s.number() );
        if let (Some(final_msn), Some(this_msn)) = (self.final_msn, this_msn) {
            if final_msn >= this_msn {
                if self.since_last_update > 1 {
                    let event = HlsEvent::ManifestStale {
                        delta: Delta {
                            before: ManifestRef { req_id: self.last_fresh_playlist_req.as_ref().unwrap().clone(), line: None },
                            after: ManifestRef { req_id: this.href.clone(), line: None }
                        },
                        since_list_update: self.since_last_update,
                    };

                    if self.since_last_update > 2 {
                        self.log.error(event)
                    } else {
                        self.log.warning(event)
                    }
                }
            } else {
                self.since_last_update = 0;
                self.last_fresh_playlist_req = Some(this.href.clone());
            }
        }
        self.since_last_update += 1;
        self.final_msn = this_msn;
    }

    fn update_timeline(&mut self, last: &PlaylistInfo, this: &PlaylistInfo) {
        self.timeline.remove_older_than(this.playlist.media_sequence);
        // media-sequence-number of the final segment in the last playlist,
        let end_msn = last.playlist.last_segment().unwrap().number();
        let skip = if end_msn >= this.playlist.media_sequence {
            1 + end_msn - this.playlist.media_sequence
        } else {
            0
        };
        self.timeline.append_new_segments(this.playlist.segments().skip(skip));
    }

    fn check_manifest_history_invariant(&mut self, last: &PlaylistInfo, this: &PlaylistInfo) {
        let skip = this.playlist.media_sequence - last.playlist.media_sequence;
        let last_segments = last.playlist.segments()
            .skip(skip);
        let this_segments = this.playlist.segments();
        let zip = last_segments.zip(this_segments);
        for (last_seg, this_seg) in zip {
            assert_eq!(last_seg.number(), this_seg.number()); // sanity check
            self.check_segment_invariant(last, this, last_seg, this_seg);
        }
    }
    fn check_segment_invariant(&mut self, last: &PlaylistInfo, this: &PlaylistInfo, last_seg: hls_m3u8::parser::MyMediaSegment, this_seg: hls_m3u8::parser::MyMediaSegment) {
        if last_seg.uri() != this_seg.uri() {
            self.log.error(HlsEvent::ManifestHistoryChangedUri {
                delta: delta(last, this),
                msn: this_seg.number(),
                last_uri: last_seg.uri().to_string(),
                this_uri: this_seg.uri().to_string(),
            });
            // Don't bother to perform other checks if the URI is different.  (It would be logical
            // for other properties to relate to the particular media segment.)
            return;
        }
        if last_seg.has_discontinuity() != this_seg.has_discontinuity() {
            println!("last,\n{:?}\nthis,\n{:?}", last_seg, this_seg);
            if this_seg.has_discontinuity() {
                self.log.error(HlsEvent::ManifestHistoryAddedDiscontinuity {
                    delta: delta(last, this),
                    msn: this_seg.number(),
                });
            } else {
                self.log.error(HlsEvent::ManifestHistoryRemovedDiscontinuity {
                    delta: delta(last, this),
                    msn: this_seg.number(),
                });
            }
        }
        if last_seg.duration() != this_seg.duration() {
            self.log.error(HlsEvent::ManifestHistoryChangedSegmentDuration {
                delta: delta(last, this),
                msn: this_seg.number(),
                last_duration_millis: last_seg.duration().duration().as_millis() as u64,
                this_duration_millis: this_seg.duration().duration().as_millis() as u64,
            });
        }
        if last_seg.byte_range() != this_seg.byte_range() {
            self.log.error(HlsEvent::ManifestHistoryChangedSegmentByterange {
                delta: delta(last, this),
                msn: this_seg.number(),
                last_byterange: last_seg.byte_range().map(|r| r.as_byte_range().to_string() ),
                this_byterange: this_seg.byte_range().map(|r| r.as_byte_range().to_string() ),
            });
        }
    }

    fn check_initial_configuration(&mut self, this: &PlaylistInfo) {
        let content_type = this.href.info().response.as_ref().unwrap().headers.get(reqwest::header::CONTENT_TYPE);
        if content_type != Some(&HeaderValue::from_static("application/vnd.apple.mpegurl")) {
            self.log.error(HlsEvent::IncorrectContentType {
                req_id: this.href.clone(),
                content_type: content_type
                    .and_then(|v| v.to_str().ok())
                    .map(ToOwned::to_owned),
            })
        }
        self.check_daterange(0, this);
    }

    fn check_headers(&mut self, this: &PlaylistInfo) {
        let headers = &this.href.info().response.as_ref().unwrap().headers;
        if let Some(age) = age(headers) {
            if std::time::Duration::from_secs(age) > this.playlist.target_duration {
                self.log.warning(HlsEvent::CachedTooLong {
                    req_id: this.href.clone(),
                    age,
                    target_duration: this.playlist.target_duration.as_secs(),
                })
            }
        }
        if let (Some(date), Some(last_modified)) = (headers.get(hyper::header::DATE).and_then(|v| v.to_str().ok() ), headers.get(hyper::header::LAST_MODIFIED).and_then(|v| v.to_str().ok() )) {
            if let (Ok(date_time), Ok(last_modified_time)) = (httpdate::parse_http_date(date), httpdate::parse_http_date(last_modified)) {
                if last_modified_time > date_time {
                    //self.log.warning(HlsEvent::LastModifiedInFuture {
                    //    req_id: this.href.clone(),
                    //    date: date.to_owned(),
                    //    last_modified: last_modified.to_owned(),
                    //})
                }
            }
        }
    }
    fn check_daterange(&mut self, last_msn: usize, this: &PlaylistInfo) {
        let mut past_ranges = HashMap::new();
        for s in this.playlist.segments() {
            if let Some(date_range) = s.date_range() {
                if s.number() > last_msn {
                    if let Some(last_range) = past_ranges.get(&date_range.id) {
                        self.check_daterange_attr_invariants(this, date_range, last_range);
                    }
                }
                past_ranges.insert(date_range.id.to_string(), date_range.clone());
            }
        }
    }
    fn check_daterange_attr_invariants(&mut self, this: &PlaylistInfo, this_range: &hls_m3u8::parser::ExtXDateRange, last_range: &hls_m3u8::parser::ExtXDateRange) {
        if let (Some(this_duration), Some(last_duration)) = (this_range.duration, last_range.duration) {
            if this_duration != last_duration {
                self.log.error(HlsEvent::DaterangeAttributeChanged {
                    req_id: this.href.clone(),
                    daterange_id: this_range.id.clone(),
                    attr_name: "DURATION".to_string(),
                    prev_value: last_duration.as_secs_f32().to_string(),
                    this_value: this_duration.as_secs_f32().to_string(),
                })
            }
        }
        if let (Some(this_start_date), Some(last_start_date)) = (this_range.start_date, last_range.start_date) {
            if this_start_date != last_start_date {
                self.log.error(HlsEvent::DaterangeAttributeChanged {
                    req_id: this.href.clone(),
                    daterange_id: this_range.id.clone(),
                    attr_name: "START-DATE".to_string(),
                    prev_value: last_start_date.to_rfc3339(),
                    this_value: this_start_date.to_rfc3339(),
                })
            }
        }
        if let (Some(this_planned_duration), Some(last_planned_duration)) = (this_range.planned_duration, last_range.planned_duration) {
            if this_planned_duration != last_planned_duration {
                self.log.error(HlsEvent::DaterangeAttributeChanged {
                    req_id: this.href.clone(),
                    daterange_id: this_range.id.clone(),
                    attr_name: "PLANNED-DURATION".to_string(),
                    prev_value: last_planned_duration.as_secs_f32().to_string(),
                    this_value: this_planned_duration.as_secs_f32().to_string(),
                })
            }
        }
        if let (Some(this_end_date), Some(last_end_date)) = (this_range.end_date, last_range.end_date) {
            if this_end_date != last_end_date {
                self.log.error(HlsEvent::DaterangeAttributeChanged {
                    req_id: this.href.clone(),
                    daterange_id: this_range.id.clone(),
                    attr_name: "END-DATE".to_string(),
                    prev_value: last_end_date.to_rfc3339(),
                    this_value: this_end_date.to_rfc3339(),
                })
            }
        }
        if let (Some(this_scte_cmd), Some(last_scte_cmd)) = (this_range.scte35_cmd.as_ref(), last_range.scte35_cmd.as_ref()) {
            if this_scte_cmd != last_scte_cmd {
                self.log.error(HlsEvent::DaterangeAttributeChanged {
                    req_id: this.href.clone(),
                    daterange_id: this_range.id.clone(),
                    attr_name: "SCTE-CMD".to_string(),
                    prev_value: last_scte_cmd.to_owned(),
                    this_value: this_scte_cmd.to_owned(),
                })
            }
        }
        if let (Some(this_scte_out), Some(last_scte_out)) = (this_range.scte35_out.as_ref(), last_range.scte35_out.as_ref()) {
            if this_scte_out != last_scte_out {
                self.log.error(HlsEvent::DaterangeAttributeChanged {
                    req_id: this.href.clone(),
                    daterange_id: this_range.id.clone(),
                    attr_name: "SCTE-OUT".to_string(),
                    prev_value: last_scte_out.to_owned(),
                    this_value: this_scte_out.to_owned(),
                })
            }
        }
        if let (Some(this_scte_in), Some(last_scte_in)) = (this_range.scte35_in.as_ref(), last_range.scte35_in.as_ref()) {
            if this_scte_in != last_scte_in {
                self.log.error(HlsEvent::DaterangeAttributeChanged {
                    req_id: this.href.clone(),
                    daterange_id: this_range.id.clone(),
                    attr_name: "SCTE-IN".to_string(),
                    prev_value: last_scte_in.to_owned(),
                    this_value: this_scte_in.to_owned(),
                })
            }
        }
    }
}

fn header_val<T: FromStr>(header: &HeaderValue) -> Option<T> {
    header.to_str().ok()?
        .parse().ok()
}

fn age(headers: &hyper::HeaderMap) -> Option<u64> {
    header_val(headers.get(hyper::header::AGE)?)
}