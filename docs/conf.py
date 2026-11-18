"""Sphinx configuration for memory-engine documentation."""

project = "memory-engine"
copyright = "2026, Michael Roynard"
author = "Michael Roynard"
release = "0.1.0"

extensions = [
    "myst_parser",
    "sphinx_copybutton",
    "sphinxcontrib.mermaid",
]

myst_enable_extensions = [
    "colon_fence",
    "deflist",
    "tasklist",
]

templates_path = ["_templates"]
exclude_patterns = [
    "_build",
    "Thumbs.db",
    ".DS_Store",
    "ROADMAP.md",
    # A branch's implementation plan, archived to an issue before merge.
    "plans",
]

html_theme = "sphinx_rtd_theme"

html_theme_options = {
    "navigation_depth": 3,
    "collapse_navigation": False,
}

myst_heading_anchors = 3
# GitHub renders a plain ```mermaid fence natively; this makes Sphinx render it too.
myst_fence_as_directive = ["mermaid"]
