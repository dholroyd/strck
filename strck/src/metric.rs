
pub trait Metric: Clone {
    fn put(&mut self, value: u64);
    fn close(self);
}