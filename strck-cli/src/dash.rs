use strck::event_log::EventSink;
use crate::{http_snoop, NullSnoop};
use futures::prelude::*;
use mp4parse::{read_mp4, MediaContext};
use std::io;
use std::convert::TryFrom;
use h264_reader::nal::pps::ParamSetId;
use reqwest::Error;
use strck::http_snoop::BodyError;

mod mpd {
    // TODO: replace this parser with something auto-generated from a schema

    use roxmltree::*;
    use crate::dash::DashManifestError;

    pub struct Mpd {
        pub periods: Vec<Period>
    }

    const NS: &str = "urn:mpeg:dash:schema:mpd:2011";

    fn el(node: &Node, name: &str) -> bool {
        if node.is_element() {
            let n = node.tag_name();
            n.name() == name && n.namespace() == Some(NS)
        } else {
            false
        }
    }
    fn parents<'a, 'input: 'a>(node: Node<'a, 'input>) -> impl Iterator<Item=roxmltree::Node<'a, 'input>> {
        std::iter::successors(node.parent(), |node| node.parent() )
    }
    fn each_child<'a, 'input: 'a>(node: Node<'a, 'input>, name: &'a str) -> impl Iterator<Item=roxmltree::Node<'a, 'input>> {
        node.children().filter_map(move |n| {
            if el(&n, name) {
                Some(n)
            } else {
                None
            }
        })
    }
    fn find_child<'a, 'input: 'a>(node: Node<'a, 'input>, name: &'a str) -> Option<roxmltree::Node<'a, 'input>> {
        node.children().find(|n| el(n, name) )
    }
    fn base_url(base: reqwest::Url, parent: roxmltree::Node) -> Result<reqwest::Url, DashManifestError> {
        let url = find_child(parent,"BaseURL").and_then(|n| {
            n.text().map(str::to_string)
        });
        if let Some(url) = url {
            base.join(&url).map_err(|e| DashManifestError::Url(e.to_string()))
        } else {
            Ok(base)
        }
    }
    impl Mpd {
        pub fn new(url: reqwest::Url, doc: roxmltree::Document) -> Result<Mpd, DashManifestError> {
            let root = doc.root_element();
            let url = base_url(url, root)?;
            let periods: Result<Vec<_>, _> = each_child(root, "Period")
                .map(|n| Period::new(url.clone(), n))
                .collect();
            let periods = periods?;

            Ok(Mpd {
                periods,
            })
        }
    }

    pub struct Period {
        pub adaptation_sets: Vec<AdaptationSet>,
    }

    impl Period {
        fn new(url: reqwest::Url, period: roxmltree::Node) -> Result<Period, DashManifestError> {
            let url = base_url(url, period)?;
            let adaptation_sets: Result<Vec<_>, _> = each_child(period, "AdaptationSet")
                .map(|n| AdaptationSet::new(url.clone(), n))
                .collect();
            let adaptation_sets = adaptation_sets?;

            Ok(Period {
                adaptation_sets,
            })
        }
    }

    pub struct AdaptationSet {
        pub representations: Vec<Representation>,
        pub segment_template: SegmentTemplate,
    }
    impl AdaptationSet {
        fn new(url: reqwest::Url, node: roxmltree::Node) -> Result<AdaptationSet, DashManifestError> {
            let url = base_url(url, node)?;
            let representations: Result<Vec<_>, _> = each_child(node, "Representation")
                .map(|n| Representation::new(url.clone(), n))
                .collect();
            let representations = representations?;

            let segment_template = SegmentTemplate::new(find_child(node, "SegmentTemplate").expect("Wanted SegmentTemplate child of AdaptationSet"));
            Ok(AdaptationSet {
                representations,
                segment_template,
            })
        }
    }

    pub struct Representation {
        pub url: reqwest::Url,
        pub id: String,
        pub bandwidth: Option<u64>,
    }
    impl Representation {
        fn new(url: reqwest::Url, node: roxmltree::Node) -> Result<Representation, DashManifestError> {
            let url = base_url(url, node)?;
            Ok(Representation {
                url,
                id: node.attribute("id").expect("id attribute").to_string(),
                bandwidth: node.attribute("bandwidth").map(|b| b.parse().expect("bandwidth attribute") ),
            })
        }
    }

    pub struct SegmentTemplate {
        pub initialization: String,
        pub media: String,
        pub segment_timeline: SegmentTimeline,
    }
    impl SegmentTemplate {
        fn new(node: roxmltree::Node) -> SegmentTemplate {
            let segment_timeline = SegmentTimeline::new(find_child(node, "SegmentTimeline").expect("SegmentTimeline element required"));
            SegmentTemplate {
                initialization: node.attribute("initialization").expect("initialization attribute").to_string(),
                media: node.attribute("media").expect("media attribute").to_string(),
                segment_timeline,
            }
        }

        pub fn initialization_uri(&self, representation: &str) -> String {
            self.initialization
                .replace("$RepresentationID$", representation)
        }

        pub fn media_uri(&self, representation: &str, time: u64) -> String {
            self.media
                .replace("$RepresentationID$", representation)
                .replace("$Time$", &format!("{}", time))
        }
    }

    pub struct SegmentTimeline {
        pub spans: Vec<Span>
    }
    impl SegmentTimeline {
        fn new(node: roxmltree::Node) -> SegmentTimeline {
            let spans = each_child(node, "S").map(|s| Span {
                t: s.attribute("t").map(|t| t.parse().expect("t attribute") ),
                d: s.attribute("d").map(|t| t.parse().expect("d attribute") ).expect("d attribute required"),
                r: s.attribute("r").map(|t| t.parse().expect("r attribute") ).unwrap_or(0),
            })
                .collect();
            SegmentTimeline {
                spans,
            }
        }

        pub fn segments(&self) -> impl Iterator<Item = Segment> +'_ {
            let mut time = 0;
            self.spans
                .iter()
                .map(move |s| {
                    if let Some(t) = s.t { time = t; }
                    let duration = s.d;
                    (0..s.r).map(move |c| Segment { time: time + c * duration, duration } )
                })
                .flatten()
        }
    }

    pub struct Span {
        pub t: Option<u64>,
        pub d: u64,
        pub r: u64,
    }
    pub struct Segment {
        pub time: u64,
        pub duration: u64,
    }
}

#[derive(Debug)]
pub enum DashManifestError {
    Http(reqwest::Error),
    NHttp(http_snoop::Error),
    Utf8(std::string::FromUtf8Error),
    Xml(roxmltree::Error),
    Url(String),
}
impl From<reqwest::Error> for DashManifestError {
    fn from(e: Error) -> Self {
        DashManifestError::Http(e)
    }
}
impl From<http_snoop::Error> for DashManifestError {
    fn from(e: http_snoop::Error) -> Self {
        DashManifestError::NHttp(e)
    }
}
pub struct DashCheck<L: EventSink> {
    client: http_snoop::Client<NullSnoop>,
    url: reqwest::Url,
    log: L,
}
impl<L: EventSink> DashCheck<L> {
    pub fn new(client: http_snoop::Client<NullSnoop>, url: reqwest::Url, log: L) -> DashCheck<L> {
        DashCheck {
            client,
            url,
            log,
        }
    }

    pub async fn start(&self) -> Result<(), DashManifestError> {
        let client = self.client.clone();
        let mpd = self.load_mpd().await?;
        let mut repr_checks = vec![];
        for p in mpd.periods {
            for a in p.adaptation_sets {
                for r in &a.representations {
                    let init = r.url
                        .join(&a.segment_template.initialization_uri(&r.id))
                        .map_err(|e| DashManifestError::Url(e.to_string()))?;
                    let segs: Result<Vec<_>, _> = a.segment_template
                        .segment_timeline
                        .segments()
                        .map(|s| {
                            r.url
                                .join(&a.segment_template.media_uri(&r.id, s.time))
                                .map_err(|e| DashManifestError::Url(e.to_string()))
                        })
                        .collect();
                    let segs = segs?;
                    println!("Will check {:?}", r.id);
                    repr_checks.push(RepresentataionCheck { id: r.id.clone(), init, segs });
                }
            }
        }
        check_all(client, repr_checks).map_err(|e| panic!("{:?}", e) ).await
    }
    pub async fn load_mpd(&self) -> Result<mpd::Mpd, DashManifestError> {
        let resp = self.client.get(self.url.clone()).send().await?;
        let url = self.url.clone();
        resp.error_for_status_ref()?;
        let body = resp.text().await?;
        roxmltree::Document::parse(&body)
            .map_err(|e| DashManifestError::Xml(e) )
            .and_then(|doc| mpd::Mpd::new(url, doc) )
    }
}

struct RepresentataionCheck {
    id: String,
    init: reqwest::Url,
    segs: Vec<reqwest::Url>,
}

#[derive(Debug)]
enum DashMediaError {
    Http(reqwest::Error),
    NHttp(http_snoop::Error),
    Body,  // TODO expose underlying disgnostics
    Mp4(mp4parse::Error),
    UnsupportedTrackCount(usize),
    ResponseSizeExceedsLimit(usize),
}
impl From<reqwest::Error> for DashMediaError {
    fn from(e: Error) -> Self {
        DashMediaError::Http(e)
    }
}
impl From<http_snoop::Error> for DashMediaError {
    fn from(e: http_snoop::Error) -> Self {
        DashMediaError::NHttp(e)
    }
}

async fn check_all(client: http_snoop::Client<NullSnoop>, repr_checks: Vec<RepresentataionCheck>) -> Result<(), DashMediaError> {
    futures::stream::iter(repr_checks)
        .map(Ok)
        .try_for_each(move |check| check_repr(client.clone(), check)).await
}

async fn check_repr(client: http_snoop::Client<NullSnoop>, check: RepresentataionCheck) -> Result<(), DashMediaError> {
    let cl = client.clone();
    let (init_data, init) = get_init(client, check.init.clone()).await?;
    // TODO: support multiplexed media (multiple tracks) later
    if init.tracks.len() == 1 {
        match init.tracks[0].track_type {
            mp4parse::TrackType::Audio | mp4parse::TrackType::Video => check_track(cl, init_data, init, check).await,
            _ => unimplemented!("track type {:?}", init.tracks[0].track_type),
        }
    } else {
        Err(DashMediaError::UnsupportedTrackCount(init.tracks.len()))
    }
}

async fn get_init(client: http_snoop::Client<NullSnoop>, url: reqwest::Url) -> Result<(bytes::Bytes, MediaContext), DashMediaError> {
    let resp = client.get(url).send().await?;
    resp.error_for_status_ref()?;
    let body = resp.bytes()
        .map_err(|e| DashMediaError::Body).await?;
    let mut read = io::Cursor::new(&body[..]);
    read_mp4(&mut read).map(|ctx| (body.clone(), ctx))
        .map_err(|e| DashMediaError::Mp4(e))
}

async fn get_seg(client: http_snoop::Client<NullSnoop>, init_data: &[u8], url: reqwest::Url) -> Result<(MediaContext, Vec<u8>), DashMediaError> {
    let mut data = init_data.to_vec();
    let resp = client.get(url).send().await?;
    resp.error_for_status_ref()?;
    let body = resp.bytes()
        .map_err(|e| DashMediaError::Body ).await?;
    // mp4parse wants the segment to be prefixed with the init segment; oblige, although this is inefficient
    data.extend_from_slice(body.as_ref());
    let mut read = io::Cursor::new(&data[..]);
    read_mp4(&mut read)
        .map_err(|e| DashMediaError::Mp4(e))
        .map(|ctx| {
            (ctx, data)
        })
}

async fn check_track(client: http_snoop::Client<NullSnoop>, init_data: bytes::Bytes, init: MediaContext, check: RepresentataionCheck) -> Result<(), DashMediaError> {
    let track = &init.tracks[0];
    match &track.stsd.as_ref().unwrap().descriptions[0] {
        mp4parse::SampleEntry::Audio(aud) => {
            println!("Audio: {:?}", aud);
        },
        mp4parse::SampleEntry::Video(vid) => {
            match &vid.codec_specific {
                mp4parse::VideoCodecSpecific::AVCConfig(avc) => {
                    let avcc = h264_reader::avcc::AvcDecoderConfigurationRecord::try_from(&avc[..]).unwrap(/*TODO*/);
                    let ctx = avcc.create_context(()).unwrap(/*TODO*/);
                    println!("  Profile indication {:?}", avcc.avc_profile_indication());
                    let sps = ctx.sps_by_id(ParamSetId::from_u32(0).unwrap());
                    println!("  {:#?}", sps);
                    let pps = ctx.pps_by_id(ParamSetId::from_u32(0).unwrap());
                    println!("  {:#?}", pps);
                },
                _ => println!("Unhandled VideoCodecSpecific")
            }
        },
        mp4parse::SampleEntry::Unknown => println!("SampleEntry::Unknown"),
    }
    for seg_url in check.segs.into_iter().take(5) {
        println!("seg {}", seg_url);
        let (seg_ctx, seg_data) = get_seg(client.clone(), &init_data[..], seg_url).await?;
        if seg_ctx.tracks.len() == 1 {
            let track_idx = 0;
            match seg_ctx.tracks[track_idx].track_type {
                mp4parse::TrackType::Audio | mp4parse::TrackType::Video => check_seg(&seg_ctx, track_idx, seg_data)?,
                _ => unimplemented!("track type {:?}", init.tracks[track_idx].track_type),
            }
        } else {
            Err(DashMediaError::UnsupportedTrackCount(init.tracks.len()))?
        }
    }
    Ok(())
}

struct SampleToChunk {
    first_sample: u32,
    first_chunk: u32,
    samples_per_chunk: u32,
}

fn check_seg(seg_ctx: &MediaContext, track_idx: usize, data: Vec<u8>) -> Result<(), DashMediaError> {
    let track = &seg_ctx.tracks[track_idx];
    let chunk_offsets = &track.stco.as_ref().unwrap().offsets;
    let sample_sizes = &track.stsz.as_ref().unwrap().sample_sizes;  // TODO: or use default for all
    let sample_to_chunk: Vec<SampleToChunk> = track.stsc.as_ref().unwrap().samples
        .iter()
        .scan((0, 0), |&mut (ref mut samples, ref mut prev_chunk), s| {
            // s.first_chunk is 1 indexed
            let first_chunk = s.first_chunk - 1;
            *samples += (first_chunk - *prev_chunk) * s.samples_per_chunk;
            *prev_chunk = first_chunk;
            Some(SampleToChunk {
                first_sample: *samples,
                first_chunk,
                samples_per_chunk: s.samples_per_chunk,
            })
        }).collect();
    for index in 0..sample_sizes.len() {
        let sample_to_chunk_idx = sample_to_chunk
            .binary_search_by_key(&index, |s| s.first_sample as usize)
            .unwrap_or_else(|i| i - 1);
        let sample_to_chunk = &sample_to_chunk[index];
        let samples_per_chunk = sample_to_chunk.samples_per_chunk as usize;
        let chunks_past_first_chunk =
            (sample_to_chunk_idx - sample_to_chunk.first_sample as usize) / samples_per_chunk;
        let samples_into_chunk = index - chunks_past_first_chunk * samples_per_chunk;
        if samples_into_chunk == 0 {
            let chunk_idx = sample_to_chunk.first_chunk as usize + chunks_past_first_chunk;
            let chunk_offset = chunk_offsets[chunk_idx as usize];
            //self.reader.seek(SeekFrom::Start(chunk_offset))?;
        }

    }
    Ok(())
}