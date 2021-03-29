use serde::Serialize;
use strck::event_log::EventSink;
use strck::hls::HlsEvent;

pub struct StderrLog {
}
impl Default for StderrLog {
    fn default() -> Self {
        StderrLog { }
    }
}
impl Clone for StderrLog {
    fn clone(&self) -> Self {
        StderrLog { }
    }
}
impl EventSink for StderrLog {
    type Extra = HlsEvent;

    fn info(&mut self, data: Self::Extra) {
        self.print(data)
    }
    fn error(&mut self, data: Self::Extra) {
        self.print(data)
    }
    fn warning(&mut self, data: Self::Extra) {
        self.print(data)
    }

    fn close(self) {  }
}

impl StderrLog {
    fn print(&mut self, data: HlsEvent) {
        eprintln!("Urk: {:?}", serde_json::to_string(&data));
        match data {
            HlsEvent::LoadedMain { req_id, variant_count } => {}
            HlsEvent::MsnGoneBackwards { .. } => {}
            HlsEvent::EndListTagRemoved => {}
            HlsEvent::UnexpectedPlaylistPropertyAddition { .. } => {}
            HlsEvent::UnexpectedPlaylistPropertyRemoval { .. } => {}
            HlsEvent::TargetDurationChanged { .. } => {}
            HlsEvent::PlaylistTypeChanged { .. } => {}
            HlsEvent::ManifestHistoryChangedUri { .. } => {}
            HlsEvent::ManifestHistoryAddedDiscontinuity { .. } => {}
            HlsEvent::ManifestHistoryRemovedDiscontinuity { .. } => {}
            HlsEvent::ManifestHistoryChangedSegmentDuration { .. } => {}
            HlsEvent::ManifestHistoryChangedSegmentByterange { .. } => {}
            HlsEvent::LiveSegmentsRemoved { .. } => {}
            HlsEvent::ManifestStale { .. } => {}
            HlsEvent::End { .. } => {}
            HlsEvent::SlowMediaManifestResponse { .. } => {}
            HlsEvent::CachedTooLong { .. } => {}
            HlsEvent::IncorrectContentType { .. } => {}
            HlsEvent::ContentTypeChanged { .. } => {}
            HlsEvent::LastModifiedInFuture { .. } => {}
            HlsEvent::HttpErrorStatus { .. } => {}
            HlsEvent::HttpTimeout { req_id } => {
                eprintln!("error[http_timeout]: timeout performing HTTP request");
                eprintln!(" --> {}", req_id.info().url);
                eprintln!("     Note: request id {:?}", blob_uuid::to_blob(&req_id.id()));
            }
            HlsEvent::MediaPlaylistWithoutExtinf { req_id, url, start, end } => {
                let mut map = codemap::CodeMap::new();
                let req = req_id;
                let file = map.add_file(req.info().url.to_string(), String::from_utf8_lossy(req.info().response.as_ref().unwrap().body.as_ref().unwrap().data.as_ref()).into_owned()).span;
                let whence = file.subspan(start as u64, end as u64);
                let label = codemap_diagnostic::SpanLabel {
                    span: whence,
                    style: codemap_diagnostic::SpanStyle::Primary,
                    label: Some("Add an #EXTINF tag before this line".to_owned())
                };
                let d = codemap_diagnostic::Diagnostic {
                    level: codemap_diagnostic::Level::Error,
                    message: "URI without #EXTINF".to_owned(),
                    code: Some("uri_without_extinf".to_owned()),
                    spans: vec![label]
                };
                let mut emitter = codemap_diagnostic::Emitter::stderr(codemap_diagnostic::ColorConfig::Auto, Some(&map));
                emitter.emit(&[d]);
            }
            HlsEvent::HttpUnknownError { .. } => {}
            HlsEvent::HttpBodyError { .. } => {}
            HlsEvent::HttpDecodeError { .. } => {}
            HlsEvent::HttpRedirectError { .. } => {}
            HlsEvent::PlaylistUtf8Error { .. } => {}
            HlsEvent::PlaylistParseError { .. } => {}
            HlsEvent::PlaylistMalformedUrl { .. } => {}
            HlsEvent::ResponseSizeExceedsLimit { .. } => {}
            HlsEvent::NumberOfRequestsExceedsLimit { .. } => {}
            HlsEvent::LastModifiedChangedButBodiesIdentical { delta, this_last_modified, last_last_modified } => {
                eprintln!(
                    "Last-Modified changed from {} to {} but response bodies where identical",
                    last_last_modified,
                    this_last_modified,
                )
            }
        }
    }
}
