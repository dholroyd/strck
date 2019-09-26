use structopt::*;
use reqwest::Url;

#[derive(StructOpt)]
#[structopt(name = "strck", about = "Media stream check")]
pub enum Strck {
    #[structopt(name = "hls", about = "Check an 'HTTP Live Streaming' endpoint")]
    Hls {
        #[structopt(help = "HLS 'Master Manifest' URL")]
        manifest: Url,
    }
}