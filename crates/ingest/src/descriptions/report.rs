//! Run-level report tallies.
//!
//! Every outcome is accounted for — `extracted + skipped_* + failed` must
//! equal the number of candidate entities. Asserted in orchestrator tests.

use serde::Serialize;

#[derive(Clone, Debug, Default, Serialize)]
pub struct Report {
    pub candidates: u64,
    pub extracted: u64,
    pub skipped_trivial: u64,
    pub skipped_private: u64,
    pub skipped_low_confidence: u64,
    pub skipped_disabled: u64,
    pub redactions_applied: u64,
    pub malformed_responses: u64,
    pub prompt_leaks_detected: u64,
    pub timeouts: u64,
    pub rate_limited: u64,
    pub transport_failures: u64,
    pub retries_total: u64,
    pub dropped_dangling_entity: u64,
    pub cost_cap_reached: bool,
    pub provider: String,
    pub model: String,
}

impl Report {
    pub fn ensure_accounted(&self) -> bool {
        let skipped = self.skipped_trivial
            + self.skipped_private
            + self.skipped_low_confidence
            + self.skipped_disabled;
        let failed = self.malformed_responses
            + self.prompt_leaks_detected
            + self.transport_failures
            + self.dropped_dangling_entity;
        self.candidates == self.extracted + skipped + failed
    }

    pub fn render(&self) -> String {
        format!(
            "description extraction report\n  provider:         {}\n  model:            {}\n  candidates:       {}\n  extracted:        {}\n  skipped_trivial:  {}\n  skipped_private:  {}\n  skipped_low_conf: {}\n  skipped_disabled: {}\n  redactions:       {}\n  malformed:        {}\n  prompt_leaks:     {}\n  timeouts:         {}\n  rate_limited:     {}\n  transport_fail:   {}\n  retries:          {}\n  dropped_dangling: {}\n  cost_cap_reached: {}",
            self.provider,
            self.model,
            self.candidates,
            self.extracted,
            self.skipped_trivial,
            self.skipped_private,
            self.skipped_low_confidence,
            self.skipped_disabled,
            self.redactions_applied,
            self.malformed_responses,
            self.prompt_leaks_detected,
            self.timeouts,
            self.rate_limited,
            self.transport_failures,
            self.retries_total,
            self.dropped_dangling_entity,
            self.cost_cap_reached,
        )
    }
}
