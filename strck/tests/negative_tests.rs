use httpmock::{MockServer};
use std::rc::Rc;
use std::cell::RefCell;
use strck::hls::HlsProcessor;

#[tokio::test]
async fn daterange_changed_attrs() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.path("/main.m3u8");
        then.status(200)
            .header("Content-Type", "application/vnd.apple.mpegurl")
            .body_from_file("tests/negative_tests/daterange_changed_attrs/main.m3u8");
    });
    server.mock(|when, then| {
        when.path("/video.m3u8");
        then.status(200)
            .header("Content-Type", "application/vnd.apple.mpegurl")
            .body_from_file("tests/negative_tests/daterange_changed_attrs/video.m3u8");
    });
    let logger = TestLog::default();
    let proc = create_test_client(&server, &logger);
    proc.start().await.unwrap();

    let events = logger.events.borrow();
    let evt = events
        .iter()
        .find(|e| matches!(e, strck::hls::HlsEvent::DaterangeAttributeChanged { .. }) );
    assert!(evt.is_some());
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
