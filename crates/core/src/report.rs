use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TechReport {
    /// A concise assessment of the technology.
    pub summary: String,
    pub pros: Vec<String>,
    pub cons: Vec<String>,
    /// Suitability score, from 0 to 100 inclusive.
    #[schemars(range(min = 0, max = 100))]
    pub score: u8,
}
