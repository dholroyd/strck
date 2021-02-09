#![type_length_limit="1400000"]

use structopt::StructOpt;
use strck::{http_snoop, hls};
use strck::http_snoop::{HttpInfo, ExtraHeader, HttpRef};
use reqwest::header::HeaderMap;
use log::*;
use futures::FutureExt;

//mod media_manifest;
mod cli;
mod dash;
mod event_log;
mod metric;

#[tokio::main]
async fn main() {
    env_logger::init();
    let cmd = cli::Strck::from_args();
    let response_limit_bytes = 40*1024*1024;  // 40MB


    let client = create_client(response_limit_bytes, cmd.extra_headers);

    let logger = event_log::StderrLog::default();

    match cmd.cmd {
        cli::Command::Hls { manifest } => {
            let ten_seconds_millis = 10 * 1000;
            let latency_metric = hdrhistogram::Histogram::new_with_max(ten_seconds_millis, 1).unwrap();
            let (media_playlist_latency, media_playlist_latency_writer) = metric::create_metric_channel("manifest_latency", latency_metric);
            let two_hours_millis = 2 * 60 * 60 * 1000;
            let stream_latency_metric = hdrhistogram::Histogram::new_with_max(two_hours_millis, 1).unwrap();
            let (stream_latency, stream_latency_writer) = metric::create_metric_channel("stream_latency", stream_latency_metric);
            let msn_regression_metric = hdrhistogram::Histogram::new_with_max(1000, 1).unwrap();
            let (msn_regression, msn_regression_writer) = metric::create_metric_channel("msn_regression", msn_regression_metric);
            let ck = hls::HlsProcessor::new(client, manifest, logger, media_playlist_latency, stream_latency, msn_regression);

            let metrics = futures::future::join_all(vec![
                media_playlist_latency_writer.consume().boxed_local(),
                stream_latency_writer.consume().boxed_local(),
                msn_regression_writer.consume().boxed_local(),
            ]);

            match futures::future::join(ck.start(), metrics).await {
                (Ok(()), _) => println!("HLS checking done"),
                (Err(e), _) => panic!("HlsCheck failed: {:?}", e),
                (_, e) => panic!("some MetricWriter failed: {:?}", e),
            }
        },
        cli::Command::Dash { manifest } => {
            error!("Sorry, DASH isn't supported right now.  One of these days!");
        }
    }
}

pub static APP_USER_AGENT: &str = concat!(
    env!("CARGO_PKG_NAME"),
    "/",
    env!("CARGO_PKG_VERSION"),
);

#[derive(Clone)]
pub struct NullSnoop;

impl http_snoop::Snoop for NullSnoop {
    fn snoop(&mut self, _event: HttpRef) { }
    fn close(self) { }
}

fn create_client(response_limit_bytes: usize, extra_headers: Vec<ExtraHeader>) -> http_snoop::Client<NullSnoop> {
    let mut header_map = HeaderMap::new();
    for h in extra_headers {
        eprintln!(" - {:?}: {:?}", h.name, h.value);
        header_map.append(h.name, h.value);
    }
    let client =
        reqwest::ClientBuilder::new()
            .timeout(std::time::Duration::from_secs(30))
            .user_agent(APP_USER_AGENT)
            .default_headers(header_map)
            .gzip(true)
            .build()
            .unwrap();

    http_snoop::Client::new(client, None, response_limit_bytes, NullSnoop)
}
