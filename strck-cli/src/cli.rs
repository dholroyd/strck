use structopt::*;
use reqwest::Url;
use std::str::FromStr;
use strck::http_snoop::ExtraHeader;

#[derive(StructOpt)]
#[structopt(name = "strck", about = "Media stream check")]
pub struct Strck {
    #[structopt(name = "extra-header", about = "Optional HTTP header to be submitted in all HTTP requests; may be specified more than once", long, number_of_values=1)]
    pub extra_headers: Vec<ExtraHeader>,
    #[structopt(subcommand)]
    pub cmd: Command,
}

#[derive(StructOpt)]
pub enum Command {
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