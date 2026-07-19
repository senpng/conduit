/// Records semantic losses that occurred while translating a request or response
/// through a codec (e.g. `ToolChoice::AnyOf` degraded to `Required` because the
/// target provider does not support `AnyOf`).
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct LossReport {
    pub warnings: Vec<LossWarning>,
}

/// A single loss entry describing one field that was degraded.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LossWarning {
    /// The canonical field path that was degraded (e.g. `"tool_choice"`).
    pub field: String,
    /// Human-readable representation of the original value (e.g. `"AnyOf([\"search\", \"calc\"])"`).
    pub original: String,
    /// Human-readable representation of the degraded value (e.g. `"Required"`).
    pub degraded_to: String,
    /// Explanation of why the degradation occurred.
    pub reason: String,
}

impl LossReport {
    /// Record a new degradation warning.
    pub fn add(
        &mut self,
        field: impl Into<String>,
        original: impl Into<String>,
        degraded_to: impl Into<String>,
        reason: impl Into<String>,
    ) {
        self.warnings.push(LossWarning {
            field: field.into(),
            original: original.into(),
            degraded_to: degraded_to.into(),
            reason: reason.into(),
        });
    }

    /// Returns `true` when no warnings have been recorded.
    pub fn is_empty(&self) -> bool {
        self.warnings.is_empty()
    }

    /// Append all warnings from `other` into this report.
    pub fn merge(&mut self, other: LossReport) {
        self.warnings.extend(other.warnings);
    }

    /// Returns the number of recorded warnings.
    pub fn len(&self) -> usize {
        self.warnings.len()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_report() {
        let r = LossReport::default();
        assert!(r.is_empty());
        assert_eq!(r.len(), 0);
    }

    #[test]
    fn add_warning() {
        let mut r = LossReport::default();
        r.add(
            "tool_choice",
            "AnyOf([\"search\", \"calc\"])",
            "Required",
            "OpenAI does not support AnyOf; degraded to Required",
        );
        assert!(!r.is_empty());
        assert_eq!(r.len(), 1);
        assert_eq!(r.warnings[0].field, "tool_choice");
        assert_eq!(r.warnings[0].degraded_to, "Required");
    }

    #[test]
    fn merge_reports() {
        let mut a = LossReport::default();
        a.add("field_a", "original_a", "degraded_a", "reason_a");

        let mut b = LossReport::default();
        b.add("field_b", "original_b", "degraded_b", "reason_b");

        a.merge(b);
        assert_eq!(a.len(), 2);
        assert_eq!(a.warnings[0].field, "field_a");
        assert_eq!(a.warnings[1].field, "field_b");
    }

    #[test]
    fn roundtrip_json() {
        let mut r = LossReport::default();
        r.add(
            "temperature",
            "2.5",
            "2.0",
            "Provider max temperature is 2.0",
        );
        let json = serde_json::to_string(&r).unwrap();
        let back: LossReport = serde_json::from_str(&json).unwrap();
        assert_eq!(back.len(), 1);
        assert_eq!(back.warnings[0].field, "temperature");
    }
}
