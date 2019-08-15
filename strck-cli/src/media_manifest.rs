#[derive(Debug)]
pub struct MediaManifestError {
    line: usize,
    kind: MediaManifestErrorKind
}

fn err(line: usize, kind: MediaManifestErrorKind) -> MediaManifestError {
    MediaManifestError {
        line,
        kind,
    }
}

#[derive(Debug)]
pub enum MediaManifestErrorKind {
    BadMediaSequenceNumber,
    BadPartDuration(String),
    MissingDurationAttribute,
    MissingUriAttribute,
}
pub struct MediaManifest {
    pub segments: Vec<Seg>,
    pub parts: Vec<Part>
}
impl MediaManifest {
    const TAG_MEDIA_SEQ: &'static str = "#EXT-X-MEDIA-SEQUENCE:";
    const TAG_PART: &'static str = "#EXT-X-PART:";

    pub fn parse(data: &str) -> Result<MediaManifest, MediaManifestError> {
        let mut segments = vec![];
        let mut parts = vec![];
        let mut next_msn = 0;
        let mut next_part_num = 0;
        for (number, l) in data.lines().enumerate() {
            if l.starts_with(Self::TAG_MEDIA_SEQ) {
                next_msn = l[Self::TAG_MEDIA_SEQ.len()..]
                    .parse()
                    .map_err(|_| err(number, MediaManifestErrorKind::BadMediaSequenceNumber))?
            } else if l.starts_with(Self::TAG_PART) {
                let mut duration = None;
                let mut uri = None;
                let attrs = l[Self::TAG_PART.len()..]
                    .split(',')
                    .map(|a| a.split_at(a.find('=').unwrap(/*TODO*/)) )
                    .map(|(k,v)| (k.trim(), v[1..].trim()) );
                for (k, v) in attrs {
                    match k {
                        "DURATION" => duration = Some(v.parse::<f32>().map_err(|_| err(number, MediaManifestErrorKind::BadPartDuration(v.to_string())) )?),
                        "URI" => uri = Some(v),
                        _ => (),
                    }
                }
                if duration.is_none() {
                    return Err(err(number, MediaManifestErrorKind::MissingDurationAttribute))
                }
                if uri.is_none() {
                    return Err(err(number, MediaManifestErrorKind::MissingUriAttribute))
                }
                let part = Part {
                    msn: next_msn,
                    part_num: next_part_num,
                    duration: duration.unwrap(),
                    uri: uri.unwrap().to_string(),
                };
                next_part_num += 1;
                parts.push(part);
            } else if l.starts_with("#") {
                // ignore
            } else {
                let seg = Seg {
                    msn: next_msn,
                };
                next_msn += 1;
                next_part_num = 0;
                segments.push(seg);
            }
        }
        Ok(MediaManifest {
            segments,
            parts,
        })
    }
}

#[derive(Debug)]
pub struct Seg {
    pub msn: u64,
}

#[derive(Debug)]
pub struct Part {
    pub msn: u64,
    pub part_num: u16,
    pub duration: f32,
    pub uri: String,
}