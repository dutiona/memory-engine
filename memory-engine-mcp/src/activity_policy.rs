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
    ActivityFilterConfig::new(
        300,
        // Ignore: formatting tools that produce no semantic value.
        ["prettier", "eslint_fix", "ruff_format", "clang_format"],
        // Promote: significant actions worth promoting to facts.
        ["git_commit", "git_push"],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_has_sensible_defaults() {
        let config = default_filter_config();
        assert_eq!(config.dedup_window_secs, 300);
        assert!(!config.ignore_patterns().is_empty());
        assert!(!config.promote_patterns().is_empty());
    }
}
