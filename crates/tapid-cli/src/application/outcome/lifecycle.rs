//! Bounded observations of checks already performed by installation.
use super::LifecycleIdentity;

pub(crate) const MAX_RECORDS: usize = 1024;

#[derive(Debug)]
pub(crate) struct LifecycleReport {
    pub(crate) coverage: &'static str,
    pub(crate) reason: Option<&'static str>,
    pub(crate) selected_packages: Option<usize>,
    pub(crate) explicitly_selected_packages: Option<usize>,
    pub(crate) impacted_packages: Option<usize>,
    pub(crate) inspected_packages: usize,
    pub(crate) packages: Vec<LifecyclePackage>,
    pub(crate) omitted_packages: usize,
    pub(crate) discovery_failures: usize,
    pub(crate) executed: usize,
    pub(crate) reused: usize,
    pub(crate) skipped: usize,
}
#[derive(Debug)]
pub(crate) struct LifecyclePackage {
    pub(crate) identity: LifecycleIdentity,
    pub(crate) discovery: &'static str,
    pub(crate) rebuild_role: Option<&'static str>,
    pub(crate) force_requested: bool,
    pub(crate) hooks: Vec<LifecycleHook>,
    pub(crate) omitted_hooks: usize,
}
#[derive(Debug)]
pub(crate) struct LifecycleHook {
    pub(crate) hook: String,
    pub(crate) authorization: &'static str,
    pub(crate) disposition: &'static str,
    pub(crate) reason: Option<&'static str>,
}
impl LifecycleReport {
    pub(crate) fn new() -> Self {
        Self {
            coverage: "not_performed",
            reason: Some("before_discovery"),
            selected_packages: None,
            explicitly_selected_packages: None,
            impacted_packages: None,
            inspected_packages: 0,
            packages: Vec::new(),
            omitted_packages: 0,
            discovery_failures: 0,
            executed: 0,
            reused: 0,
            skipped: 0,
        }
    }
    pub(crate) fn begin(&mut self, count: usize) {
        self.selected_packages = Some(count);
        self.coverage = "incomplete";
        self.reason = Some("processing_stopped");
    }
    pub(crate) fn package(&mut self, identity: LifecycleIdentity) -> Option<usize> {
        if self.packages.len() == MAX_RECORDS {
            self.omitted_packages += 1;
            return None;
        }
        let index = self.packages.len();
        self.packages.push(LifecyclePackage {
            identity,
            discovery: "not_performed",
            rebuild_role: None,
            force_requested: false,
            hooks: Vec::new(),
            omitted_hooks: 0,
        });
        Some(index)
    }
    pub(crate) fn discovered(&mut self, package: Option<usize>, succeeded: bool) {
        self.inspected_packages += 1;
        self.discovery_failures += usize::from(!succeeded);
        if let Some(index) = package {
            self.packages[index].discovery = if succeeded { "succeeded" } else { "failed" };
        }
    }
    pub(crate) fn hook(&mut self, package: Option<usize>, hook: &str) -> Option<(usize, usize)> {
        let package = package?;
        let record = &mut self.packages[package];
        if record.hooks.len() == MAX_RECORDS {
            record.omitted_hooks += 1;
            return None;
        }
        let index = record.hooks.len();
        record.hooks.push(LifecycleHook {
            hook: hook.into(),
            authorization: "not_evaluated",
            disposition: "not_performed",
            reason: Some("processing_stopped"),
        });
        Some((package, index))
    }
    pub(crate) fn authorization(
        &mut self,
        index: Option<(usize, usize)>,
        authorization: &'static str,
    ) {
        if let Some((package, hook)) = index {
            self.packages[package].hooks[hook].authorization = authorization;
        }
    }
    pub(crate) fn disposition(
        &mut self,
        index: Option<(usize, usize)>,
        disposition: &'static str,
        reason: Option<&'static str>,
    ) {
        match disposition {
            "executed" => self.executed += 1,
            "reused" => self.reused += 1,
            "skipped" => self.skipped += 1,
            _ => {}
        }
        if let Some((package, hook)) = index {
            let record = &mut self.packages[package].hooks[hook];
            record.disposition = disposition;
            record.reason = reason;
        }
    }
    pub(crate) fn finish(&mut self) {
        if self.discovery_failures == 0 {
            self.coverage = "complete";
            self.reason = None;
        } else {
            self.reason = Some("discovery_failed");
        }
    }
}
