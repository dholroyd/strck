use serde_derive::Serialize;

// Workaround for serde_dynamodb producing `severity: {___enum_tag: "info"}` etc.
fn ser_simple_enum<S>(sev: &EventSeverity, s: S) -> Result<S::Ok, S::Error> where S: serde::Serializer {
    match sev {
        EventSeverity::Info => s.serialize_str("info"),
        EventSeverity::Warning => s.serialize_str("warning"),
        EventSeverity::Error => s.serialize_str("error"),
    }
}

#[derive(Serialize)]
pub struct CheckEvent<Extra: serde::Serialize> {
    #[serde(rename="entity_jobId")]
    pub entity_job_id: String,
    pub time_id: String,
    #[serde(serialize_with = "ser_simple_enum")]
    pub severity: EventSeverity,
    pub expires: u64,

    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Serialize, Debug, Clone, Copy)]
pub enum EventSeverity {
    #[serde(rename="info")]
    Info,
    #[serde(rename="warning")]
    Warning,
    #[serde(rename="error")]
    Error,
}

pub trait EventSink: Clone {
    type Extra: serde::Serialize;

    // TODO: We've got these separate methods per severity-level, and we also have a separate
    //       type/variant per message; the severity is almost always inherent in the value of the
    //       concrete type Self::Extra.  Refactor this interface to remove the duplication.

    fn info(&mut self, data: Self::Extra);
    fn error(&mut self, data: Self::Extra);
    fn warning(&mut self, data: Self::Extra);

    fn close(self);
}