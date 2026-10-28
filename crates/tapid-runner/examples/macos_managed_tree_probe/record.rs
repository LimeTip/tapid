//! Research observations cannot construct production enforcement receipts.
use serde::Serialize;

#[derive(Debug, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Verdict {
    HarnessFailure,
    Counterexample,
    Unsupported,
    Unverified,
}

#[derive(Serialize)]
pub struct Observation<'a> {
    pub case: &'a str,
    pub candidate: &'a str,
    pub requested: &'a str,
    pub available: &'a str,
    pub observed: String,
    pub enforced: &'a str,
    pub threshold: u64,
    pub units: &'a str,
    pub setup_ready: bool,
    pub positive_control: bool,
    pub candidate_available: bool,
    pub violation_observed: bool,
    pub candidate_cleanup: &'a str,
    pub rescue: &'a str,
    pub cleanup_error: Option<String>,
}

impl<'a> Observation<'a> {
    pub fn diagnostic(case: &'a str, requested: &'a str) -> Self {
        Self {
            case,
            candidate: "unsandboxed-diagnostic",
            requested,
            available: "process observations only; no authoritative boundary",
            observed: String::new(),
            enforced: "none",
            threshold: 300,
            units: "milliseconds delayed work; not an enforced limit",
            setup_ready: false,
            positive_control: false,
            candidate_available: true,
            violation_observed: false,
            candidate_cleanup: "none",
            rescue: "finite child workload; not candidate cleanup",
            cleanup_error: None,
        }
    }

    pub fn verdict(&self) -> Verdict {
        if self.cleanup_error.is_some() {
            Verdict::HarnessFailure
        } else if !self.candidate_available {
            Verdict::Unsupported
        } else if !self.setup_ready || !self.positive_control {
            Verdict::HarnessFailure
        } else if self.violation_observed {
            Verdict::Counterexample
        } else {
            // Absence of a marker and sampled PIDs never prove enforcement.
            Verdict::Unverified
        }
    }

    pub fn emit(&self) -> std::io::Result<()> {
        #[derive(Serialize)]
        struct Result<'a, 'b> {
            verdict: Verdict,
            observation: &'a Observation<'b>,
        }
        let text = toml::to_string(&Result {
            verdict: self.verdict(),
            observation: self,
        })
        .map_err(std::io::Error::other)?;
        println!("--- research case ---\n{text}");
        Ok(())
    }
}
