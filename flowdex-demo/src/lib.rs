pub mod parser;
pub mod render;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Severity {
    Low,
    Medium,
    High,
    Critical,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Incident {
    pub title: String,
    pub severity: Severity,
    pub owner: String,
}
