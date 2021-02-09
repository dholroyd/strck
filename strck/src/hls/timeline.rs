use std::fmt;
use hls_m3u8::parser::MyMediaSegment;

#[derive(Default, Debug)]
pub struct Timeline {
    sequences: Vec<Sequence>,
}
impl Timeline {
    /// remove timeline content older than the given _media sequence number_
    pub fn remove_older_than(&mut self, msn: usize) {
        self.sequences.retain(|s| {
            s.last_msn >= msn
        });
        if let Some(s) = self.sequences.first_mut() {
            s.remove_older_than(msn);
        }
    }

    pub fn append_new_segments<'a>(&mut self, segments: impl Iterator<Item=MyMediaSegment<'a>>) {
        for s in segments {
            if self.sequences.is_empty() || s.has_discontinuity() {
                // segment starts a new sequence,
                let seq = Sequence {
                    first_msn: s.number(),
                    last_msn: s.number(),
                };
                self.sequences.push(seq)
            } else {
                let last = self.sequences.last_mut().unwrap();  // we checked is_empty() above
                if last.last_msn + 1 != s.number() {
                    //println!("Unexpected MSN got={} expected={}\n    {:?}", s.number(), last.last_msn + 1, s);
                }
                last.last_msn = s.number();
            }
        }
    }
}

/// the HLS timeline might be continuous from the very start to the very end of the timeline, but
/// it is also possible for the timeline to include 'discontinuities', in which case the timeline
/// will be composed of multiple sequences, separated at the discontinuities.
struct Sequence {
    first_msn: usize,
    last_msn: usize,
}
impl Sequence {
    /// remove timeline content older than the given _media sequence number_
    fn remove_older_than(&mut self, msn: usize) {
        if self.first_msn < msn {
            self.first_msn = msn;
        }
        self.assert_invariants();
    }

    fn assert_invariants(&self) {
        assert!(self.first_msn <= self.last_msn)
    }
}
impl fmt::Debug for Sequence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Sequence({}..{})", self.first_msn, self.last_msn)
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use hls_m3u8::parser::MyMediaPlaylist;

    #[test]
    fn remove() {
        let pl =
            b"#EXTM3U
#EXT-X-VERSION:3
#EXT-X-TARGETDURATION:4
#EXTINF:1,
foo
#EXTINF:1,
foo";
        let parser = hls_m3u8::parser::Parser::new(hls_m3u8::parser::Cursor::from(&pl[..]));
        let playlist = parser.parse().unwrap().build().unwrap();
        let mut timeline = Timeline::default();
        timeline.append_new_segments(playlist.segments());
        assert_eq!(timeline.sequences.len(), 1);
        assert_eq!(timeline.sequences[0].first_msn, 0);
        assert_eq!(timeline.sequences[0].last_msn, 1);
        timeline.remove_older_than(1);
        assert_eq!(timeline.sequences.len(), 1);
        assert_eq!(timeline.sequences[0].first_msn, 1);
    }
}