//! Claude Code adapter-specific activity filter policy.
//!
//! Provides default ignore/promote patterns for Claude Code tool streams.
//! This keeps adapter-specific tool-name knowledge out of the core engine crate.

use memory_engine::ActivityFilterConfig;

/// Default activity filter configuration for Claude Code tool streams.
///
/// - **Ignore:** formatting-only tools and lint auto-fixes
/// - **Promote:** git commits, test failures, new file creation
/// - **Dedup window:** 300 seconds (5 minutes)
#[must_use]
pub fn default_filter_config() -> ActivityFilterConfig {
    ActivityFilterConfig {
        dedup_window_secs: 300,
        ignore_patterns: vec![
            // Formatting tools that produce no semantic value
            "prettier".into(),
            "eslint_fix".into(),
            "ruff_format".into(),
            "clang_format".into(),
        ],
        promote_patterns: vec![
            // Significant actions worth promoting to facts
            "git_commit".into(),
            "git_push".into(),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_has_sensible_defaults() {
        let config = default_filter_config();
        assert_eq!(config.dedup_window_secs, 300);
        assert!(!config.ignore_patterns.is_empty());
        assert!(!config.promote_patterns.is_empty());
    }
}
