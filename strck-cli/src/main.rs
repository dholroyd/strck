use crate::media_manifest::MediaManifest;
use structopt::StructOpt;
use futures::prelude::*;
use reqwest::Error;
use hls_m3u8::tags::VariantStream;

mod media_manifest;
mod cli;
mod dash;

#[tokio::main]
async fn main() {
    let cmd = cli::Strck::from_args();

    match cmd {
        cli::Strck::Hls { manifest } => {
            let client = create_client(false);
            let log = StderrLog;
            let ck = HlsCheck::new(client, manifest, log);

            match ck.start().await {
                Ok(_) => (),
                Err(e) => panic!("Core::run() failed: {:?}", e),
            }
        },
        cli::Strck::Dash { manifest } => {
            let client = create_client(false);
            let log = StderrLog;
            let ck = dash::DashCheck::new(client, manifest, log);

            match ck.start().await {
                Ok(_) => (),
                Err(e) => panic!("Core::run() failed: {:?}", e),
            }
        }
    }
}

fn create_client(h2: bool) -> reqwest::Client {
    let mut headers = reqwest::header::HeaderMap::new();
    let agent = format!("strck {}.{}.{}{}",
                        env!("CARGO_PKG_VERSION_MAJOR"),
                        env!("CARGO_PKG_VERSION_MINOR"),
                        env!("CARGO_PKG_VERSION_PATCH"),
                        option_env!("CARGO_PKG_VERSION_PRE").unwrap_or(""));
    headers.append(reqwest::header::USER_AGENT, agent.parse().unwrap());
    let b =
        reqwest::ClientBuilder::new()
            .timeout(std::time::Duration::from_secs(30))
            .default_headers(headers);
    let b = if h2 {
        b.http2_prior_knowledge()
    } else {
        b
    };
    let client = b.build()
            .unwrap();
    client
}

enum ManifestEvent {
    UnknownGroup {
        group_id: String,
    }
}
pub trait ManifestEventLog {

}

struct StderrLog;
impl ManifestEventLog for StderrLog {

}

#[derive(Debug)]
enum HlsManifestError {
    Http(reqwest::Error),
    Utf8(std::string::FromUtf8Error),
    Parse(hls_m3u8::Error),
    Url(url::ParseError),
    MediaManifest(media_manifest::MediaManifestError),
}
impl From<reqwest::Error> for HlsManifestError {
    fn from(e: Error) -> Self {
        HlsManifestError::Http(e)
    }
}
impl From<url::ParseError> for HlsManifestError {
    fn from(e: url::ParseError) -> Self {
        HlsManifestError::Url(e)
    }
}

struct HlsCheck<L: ManifestEventLog> {
    client: reqwest::Client,
    url: reqwest::Url,
    log: L,
}
impl<L: ManifestEventLog> HlsCheck<L> {
    fn new(client: reqwest::Client, url: reqwest::Url, log: L) -> HlsCheck<L> {
        HlsCheck {
            client,
            url,
            log,
        }
    }

    pub async fn start(&self) -> Result<(), HlsManifestError> {
        let url = self.url.clone();
        let client = self.client.clone();
        let master = self.load_master_manifest().await?;
        let variant_urls: Result<Vec<_>, _> = master.variant_streams.iter().map(|var| {
            match var {
                VariantStream::ExtXIFrame { uri, .. } | VariantStream::ExtXStreamInf { uri, .. } => {
                    url.join(uri)
                },
            }
        }).collect();
        let mut variant_urls = variant_urls?;

        let media_urls: Result<Vec<_>, _> = master.media.iter().filter_map(|media| media.uri().map(|u| url.join(u)) ).collect();
        let media_urls = media_urls?;
        variant_urls.extend(media_urls);

        futures::future::join_all(
            variant_urls.into_iter().map(move |u| {
                process_media_manifest(client.clone(), u)
            })
        ).await;
        Ok(())
    }

    async fn load_master_manifest(&self) -> Result<hls_m3u8::MasterPlaylist, HlsManifestError>{
        let req = self.client.get(self.url.clone()).build().unwrap();
        let resp = self.client.execute(req).await?;
        resp.error_for_status_ref()?;
        // TODO: enforce size limit on manifest response to avoid memory exhaustion
        let body = resp.text().await?;

        body.parse::<hls_m3u8::MasterPlaylist>()
            .map_err(|e| HlsManifestError::Parse(e) )
    }
}

fn poll_media_manifest(client: reqwest::Client, url: reqwest::Url) -> impl Stream<Item=Result<(std::time::Duration, MediaManifest), HlsManifestError>> {
    // TODO:
    //      - allow polling to be cancelled
    //      - rate limit in case server-side blocking doesn't happen
    //      - handle errors; retry on a sensible schedule
    futures::stream::try_unfold(None, move |msn_and_part_num| {
        let mut url = url.clone();
        if let Some((msn, last_part_num)) = msn_and_part_num {
            let mut new_query = url.query().map_or(String::new(), |s| s.to_string());
            let push_num = 1;
            if !new_query.is_empty() {
                new_query.push_str("&");
            }
            new_query.push_str(&format!(
                "_HLS_msn={}&_HLS_part={}&_HLS_push={}",
                msn,
                last_part_num + 1,
                push_num
            ));
            url.set_query(Some(&new_query));
        }
        let start = std::time::Instant::now();
        load_media_manifest(client.clone(), url.clone()).map_ok(move |manifest| {
            println!("url {:?}", url);
            let duration = std::time::Instant::now().duration_since(start);
            let last_part = manifest.parts.last().expect("At least one part" /* TODO */);
            let msn = last_part.msn;
            let part_num = last_part.part_num;
            Some(((duration, manifest), Some((msn, part_num))))
        })
    })
}

async fn process_media_manifest(client: reqwest::Client, url: reqwest::Url) -> Result<(), HlsManifestError> {
    poll_media_manifest(client, url).try_for_each(|(duration, manifest)| {
        let last_part = manifest.parts.last().expect("At least one part" /* TODO */);
        let part_duration = std::time::Duration::from_micros((last_part.duration * 1_000_000.0) as u64);
        if duration > part_duration {
            eprintln!("Blocking manifest reload took {}ms longer than part duration: {}ms", duration.as_millis() - part_duration.as_millis(), duration.as_millis())
        }
//            if last_part.msn == msn {
//                if last_part.part_num != last_part_num + 1 {
//                    eprintln!("Expected part {} to be available, but got {} (MSN={})", last_part_num + 1, last_part.part_num, msn);
//                }
//            } else if last_part.msn == msn + 1 {
//                if last_part.part_num != 0 {
//                    eprintln!("Expected part 0, but got {}, after MSN changed from {} to {}", last_part.part_num, msn, last_part.msn);
//                }
//            }
        future::ready(Ok(()))
    }).await
}
async fn load_media_manifest(client: reqwest::Client, url: reqwest::Url) -> Result<MediaManifest, HlsManifestError> {
    let req = client.get(url).build().unwrap();
    let resp = client.execute(req).await?;
    resp.error_for_status_ref()?;
    // TODO: enforce size limit on manifest response to avoid memory exhaustion
    let body = resp.text().await?;

    media_manifest::MediaManifest::parse(&body)
        .map_err(|e| HlsManifestError::MediaManifest(e) )
}

struct Media {
    url: reqwest::Url,
}
impl Media {

}