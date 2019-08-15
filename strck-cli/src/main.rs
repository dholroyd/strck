use futures::future::Future;
use futures::stream::Stream;
use core::borrow::Borrow;
use tokio_core::reactor;
use crate::media_manifest::MediaManifest;

mod media_manifest;

fn main() {
    let mut core = reactor::Core::new().unwrap();
    let url = std::env::args().nth(1).expect("URL must be given");

    let client = create_client();
    let u = reqwest::Url::parse(&url).expect("Invalid URL");
    let log = StderrLog;
    let ck = HlsCheck::new(client, u, log);

    let handle = core.handle();
    let fut = ck.start(handle);
    match core.run(fut) {
        Ok(_) => (),
        Err(e) => panic!("Core::run() failed: {:?}", e),
    }
}

fn create_client() -> reqwest::r#async::Client {
    let mut headers = reqwest::header::HeaderMap::new();
    let agent = format!("strck {}.{}.{}{}",
                        env!("CARGO_PKG_VERSION_MAJOR"),
                        env!("CARGO_PKG_VERSION_MINOR"),
                        env!("CARGO_PKG_VERSION_PATCH"),
                        option_env!("CARGO_PKG_VERSION_PRE").unwrap_or(""));
    headers.append(reqwest::header::USER_AGENT, agent.parse().unwrap());
    let client =
        reqwest::r#async::ClientBuilder::new()
            .timeout(std::time::Duration::from_secs(30))
            .h2_prior_knowledge()
            .default_headers(headers)
            .build()
            .unwrap();
    client
}

enum ManifestEvent {
    UnknownGroup {
        group_id: String,
    }
}
trait ManifestEventLog {

}

struct StderrLog;
impl ManifestEventLog for StderrLog {

}

#[derive(Debug)]
enum HlsManifestError {
    Http(reqwest::Error),
    Utf8(std::string::FromUtf8Error),
    Parse(hls_m3u8::Error),
    MediaManifest(media_manifest::MediaManifestError),
}

struct HlsCheck<L: ManifestEventLog> {
    client: reqwest::r#async::Client,
    url: reqwest::Url,
    log: L,
}
impl<L: ManifestEventLog> HlsCheck<L> {
    fn new(client: reqwest::r#async::Client, url: reqwest::Url, log: L) -> HlsCheck<L> {
        HlsCheck {
            client,
            url,
            log,
        }
    }

    pub fn start(&self, handle: reactor::Handle) -> impl Future<Item=(), Error=HlsManifestError> {
        let url = self.url.clone();
        let client = self.client.clone();
        self.load_master_manifest()
            .and_then(move |master| {
                let tags = master.stream_inf_tags().to_vec();
                futures::future::join_all(
                    tags.into_iter().map(move |inf| {
                        let u = url.join(inf.uri()).unwrap(/*TODO*/);
                        println!("stream inf {}", u);
                        process_media_manifest(client.clone(), u)
                    })
                )
            })
            .map(|v| () )
    }

    fn load_master_manifest(&self) -> impl Future<Item=hls_m3u8::MasterPlaylist, Error=HlsManifestError>{
        let req = self.client.get(self.url.clone()).build().unwrap();
        self.client
            .execute(req)
            .and_then(|resp| {
                resp.error_for_status()
            })
            .and_then(|resp| {
                // TODO: enforce size limit on manifest response to avoid memory exhaustion
                resp.into_body()
                    .fold(vec![], |mut v, c| { v.extend_from_slice(c.borrow()); futures::future::ok::<_, reqwest::Error>(v) } )
            })
            .map_err(|e| HlsManifestError::Http(e) )
            .and_then(|body| {
                String::from_utf8(body)
                    .map_err(|e| HlsManifestError::Utf8(e))
            })
            .and_then(|body| {
                body.parse::<hls_m3u8::MasterPlaylist>()
                    .map_err(|e| HlsManifestError::Parse(e) )
            })
    }
}

fn process_media_manifest(client: reqwest::r#async::Client, url: reqwest::Url) -> impl Future<Item=(), Error=HlsManifestError> {
    load_media_manifest(client.clone(), url.clone())
        .and_then(|manifest| {
            let last_part = manifest.parts.last().expect("At least one part" /* TODO */);
            futures::future::loop_fn((last_part.msn, last_part.part_num), move |(msn, last_part_num)| {
                let mut url = url.clone();
                let mut new_query = url.query().map_or(String::new(), |s| s.to_string() );
                let push_num = 1;
                if !new_query.is_empty() {
                    new_query.push_str("&");
                }
                new_query.push_str(&format!(
                    "_HLS_msn={}&_HLS_part={}&_HLS_push={}",
                    msn,
                    last_part_num+1,
                    push_num
                ));
                url.set_query(Some(&new_query));
                let start = std::time::Instant::now();
                load_media_manifest(client.clone(), url)
                    .and_then(move |manifest| {
                        let duration = std::time::Instant::now().duration_since(start);
                        let last_part = manifest.parts.last().expect("At least one part" /* TODO */);
                        let part_duration = std::time::Duration::from_micros((last_part.duration * 1_000_000.0) as u64);
                        if duration > part_duration {
                            eprintln!("Blocking manifest reload took {}ms longer than part duration: {}ms", duration.as_millis()-part_duration.as_millis(), duration.as_millis())
                        }
                        if last_part.msn == msn {
                            if last_part.part_num != last_part_num + 1 {
                                eprintln!("Expected part {} to be available, but got {} (MSN={})", last_part_num + 1, last_part.part_num, msn);
                            }
                        } else if last_part.msn == msn + 1 {
                            if last_part.part_num != 0 {
                                eprintln!("Expected part 0, but got {}, after MSN changed from {} to {}", last_part.part_num, msn, last_part.msn);
                            }
                        }
                        Ok(futures::future::Loop::Continue((last_part.msn, last_part.part_num)))
                    })
            })
        })
}
fn load_media_manifest(client: reqwest::r#async::Client, url: reqwest::Url) -> impl Future<Item=MediaManifest, Error=HlsManifestError> {
    let req = client.get(url).build().unwrap();
    client
        .execute(req)
        .and_then(|resp| {
            resp.error_for_status()
        })
        .and_then(|resp| {
            // TODO: enforce size limit on manifest response to avoid memory exhaustion
            resp.into_body()
                .fold(vec![], |mut v, c| { v.extend_from_slice(c.borrow()); futures::future::ok::<_, reqwest::Error>(v) } )
        })
        .map_err(|e| HlsManifestError::Http(e) )
        .and_then(|body| {
            String::from_utf8(body)
                .map_err(|e| HlsManifestError::Utf8(e))
        })
        .and_then(|body| {
            media_manifest::MediaManifest::parse(&body)
                .map_err(|e| HlsManifestError::MediaManifest(e) )
        })
}

struct Media {
    url: reqwest::Url,
}
impl Media {

}