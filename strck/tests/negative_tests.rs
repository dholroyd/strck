use httpmock::{MockServer};
use std::rc::Rc;
use std::cell::RefCell;
use strck::hls::{HlsEvent, HlsProcessor};

#[tokio::test]
async fn daterange_changed_attrs() {
    let cfgs = vec![
        MockCfg{route: "/main.m3u8", path: "tests/negative_tests/daterange_changed_attrs/main.m3u8"},
        MockCfg{route: "/video.m3u8", path: "tests/negative_tests/daterange_changed_attrs/video.m3u8"}
    ];
    let server = create_test_server(&cfgs);
    let logger = TestLog::default();
    let proc = create_test_client(&server, &logger);
    proc.start().await.unwrap();

    let events = logger.events.borrow();
    let evt = events
        .iter()
        .find(|e| matches!(e, strck::hls::HlsEvent::DaterangeAttributeChanged { .. }) );
    assert!(evt.is_some());
}

#[tokio::test]
async fn valid_segment_durations() {
    let cfgs = vec![
        MockCfg{route: "/main.m3u8", path: "tests/negative_tests/excessive_segment_durations/main.m3u8"},
        MockCfg{route: "/video.m3u8", path: "tests/negative_tests/excessive_segment_durations/video.m3u8"}
    ];
    let server = create_test_server(&cfgs);
    let logger = TestLog::default();
    let proc = create_test_client(&server, &logger);
    proc.start().await.unwrap();

    let events = logger.events.borrow();
    let excessive_segments: Vec<&HlsEvent> = events
        .iter()
        .filter(|e| matches!(e, strck::hls::HlsEvent::ExcessiveSegmentDuration { .. }) )
        .collect();

    assert_eq!(excessive_segments.len(), 2);
}

#[derive(Debug)]
struct MockCfg<'a> {
    route: &'a str,
    path: &'a str,
}

fn create_test_server(cfgs: &Vec<MockCfg> ) -> MockServer {
    let server = MockServer::start();
    for cfg in cfgs {
        server.mock(|when, then|{
            when.path(cfg.route);
            then.status(200)
            .header("Content-Type", "application/vnd.apple.mpegurl")
            .body_from_file(cfg.path);
        });
    }
    server
}

fn create_test_client(server: &MockServer, logger: &TestLog) -> HlsProcessor<NullSnoop, TestLog, TestMetric> {
    let client = create_client();
    let proc = strck::hls::HlsProcessor::new(
        client,
        reqwest::Url::parse(&server.url("/main.m3u8")).unwrap(),
        logger.clone(),
        TestMetric,
        TestMetric,
        TestMetric,
    );
    proc
}

#[derive(Clone)]
struct TestMetric;
impl strck::metric::Metric for TestMetric {
    fn put(&mut self, _value: u64) { }
    fn close(self) { }
}

#[derive(Clone)]
struct TestLog {
    events: Rc<RefCell<Vec<strck::hls::HlsEvent>>>,
}
impl Default for TestLog {
    fn default() -> Self {
        TestLog {
            events: Rc::new(RefCell::new(vec![]))
        }
    }
}
impl strck::event_log::EventSink for TestLog {
    type Extra = strck::hls::HlsEvent;

    fn info(&mut self, data: Self::Extra) {
        self.events.borrow_mut().push(data);
    }
    fn error(&mut self, data: Self::Extra) {
        self.events.borrow_mut().push(data);
    }
    fn warning(&mut self, data: Self::Extra) {
        self.events.borrow_mut().push(data);
    }

    fn close(self) {  }
}

fn create_client() -> strck::http_snoop::Client<NullSnoop> {
    let client =
        reqwest::ClientBuilder::new()
            .timeout(std::time::Duration::from_secs(30))
            .gzip(true)
            .build()
            .unwrap();

    let response_limit_bytes = 20 * 1024 * 1024;
    strck::http_snoop::Client::new(client, None, response_limit_bytes, NullSnoop)
}
#[derive(Clone)]
pub struct NullSnoop;

impl strck::http_snoop::Snoop for NullSnoop {
    fn snoop(&mut self, _event: strck::http_snoop::HttpRef) { }
    fn close(self) { }
}
