use structopt::*;
use reqwest::Url;

#[derive(StructOpt)]
#[structopt(name = "strck", about = "Media stream check")]
pub enum Strck {
    #[structopt(name = "hls", about = "Check an HTTP Live Streaming master manifest")]
    Hls {
        manifest: Url,
    }
}