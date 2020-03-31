use structopt::*;
use reqwest::Url;

#[derive(StructOpt)]
#[structopt(name = "strck", about = "Media stream check")]
pub enum Strck {
    #[structopt(name = "hls", about = "Check an 'HTTP Live Streaming' endpoint")]
    Hls {
        #[structopt(help = "HLS 'Master Manifest' URL")]
        manifest: Url,
    },
    #[structopt(name = "dash", about = "Check a 'Dynamic Adaptive Streaming over HTTP' manifest")]
    Dash {
        #[structopt(help = "DASH 'Media Presentation Descriptor' URL")]
        manifest: Url,
    },
}