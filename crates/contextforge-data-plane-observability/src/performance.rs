use std::time::Instant;

/// Emits one structured latency event when it leaves scope.
#[must_use = "the timer must remain in scope for the operation being measured"]
pub struct PerformanceTimer {
    started_at: Instant,
    component: String,
    operation: String,
    metric: &'static str,
    outcome: &'static str,
}

impl PerformanceTimer {
    pub fn database(component: impl Into<String>, operation: impl Into<String>) -> Self {
        Self::new(component, operation, "database_latency")
    }

    pub fn external_call(component: impl Into<String>, operation: impl Into<String>) -> Self {
        Self::new(component, operation, "external_call_latency")
    }

    pub fn queue_wait(component: impl Into<String>, operation: impl Into<String>) -> Self {
        Self::new(component, operation, "queue_wait_latency")
    }

    fn new(component: impl Into<String>, operation: impl Into<String>, metric: &'static str) -> Self {
        Self {
            started_at: Instant::now(),
            component: component.into(),
            operation: operation.into(),
            metric,
            outcome: "unknown",
        }
    }

    pub fn succeeded(&mut self) {
        self.outcome = "success";
    }

    pub fn failed(&mut self) {
        self.outcome = "error";
    }

    pub fn record_result<T, E>(&mut self, result: &Result<T, E>) {
        if result.is_ok() {
            self.succeeded();
        } else {
            self.failed();
        }
    }
}

impl Drop for PerformanceTimer {
    fn drop(&mut self) {
        let latency_ms = u64::try_from(self.started_at.elapsed().as_millis()).unwrap_or(u64::MAX);
        tracing::info!(
            component = self.component,
            operation = self.operation,
            event_type = "PERFORMANCE",
            metric = self.metric,
            outcome = self.outcome,
            latency_ms,
            "operation latency recorded"
        );
    }
}
