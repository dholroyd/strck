use hdrhistogram;
use futures::channel::mpsc::{Sender, Receiver};
use futures::StreamExt;

pub fn create_metric_channel(metric_name: &str, metric: hdrhistogram::Histogram<u64>) -> (Metric, MetricWriter) {
    let (tx, rx) = futures::channel::mpsc::channel(40);
    (Metric { tx }, MetricWriter { metric_name: metric_name.to_owned(), metric, rx })
}

enum MetricEvent {
    Closedown,
    Metric(u64),
}

pub struct MetricWriter {
    metric_name: String,
    metric: hdrhistogram::Histogram<u64>,
    rx: Receiver<MetricEvent>,
}
impl MetricWriter {
    pub async fn consume(mut self) -> Result<(), ()> {
        while let Some(item) = self.rx.next().await {
            match item {
                MetricEvent::Closedown => {
                    self.rx.close();
                    self.dump();
                    break;
                },
                MetricEvent::Metric(val) => {
                    if let Err(e) = self.metric.record(val) {
                        eprintln!("couldn't add metric value to histogram {:?}", val);
                    }
                },
            }
        }
        Ok(())
    }

    fn dump(&self) {
        if let Some(max) = self.metric.iter_all().map(|val| val.count_at_value() ).max() {
            let count = self.metric.len();
            println!("Metric {}, {} samples, {} max", self.metric_name, count, max);
            if max > 0 {
                let mut val = 0;
                for i in self.metric.iter_all() {
                    let bar_size = i.count_at_value() * 50 / max;
                    println!("{:>7} {} {}", i.value_iterated_to(), "#".repeat(bar_size as usize), i.count_at_value());
                    val += i.count_at_value();
                    if val >= count {
                        break;
                    }
                }
            }
        }
    }
}

#[derive(Clone)]
pub struct Metric {
    tx: Sender<MetricEvent>,
}
impl strck::metric::Metric for Metric {
    fn put(&mut self, value: u64) {
        let res = self.tx.try_send(MetricEvent::Metric(value));
        if let Err(e) = res {
            if e.is_full() {
                println!("Not storing metric; queue full")
            }
            if e.is_disconnected() {
                println!("Not storing metric; writer disconnected")
            }
        }
    }

    fn close(mut self) {
        if let Err(e) = self.tx.try_send(MetricEvent::Closedown) {
            if e.is_full() {
                // FIXME: probably need a separate oneshot, rather than signalling in-band
                panic!("cant tell metric channel to close down; queue full")
            }
        }
        self.tx.close_channel();
    }
}