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

suppress_warnings = ["myst.header"]

templates_path = ["_templates"]
exclude_patterns = [
    "_build",
    "Thumbs.db",
    ".DS_Store",
    # Directories not part of the Sphinx tree (move to autonomous-agent-project later)
    "research/**",
    "prompts/**",
    "logs/**",
    "papers/**",
    "ROADMAP.md",
]

html_theme = "sphinx_rtd_theme"
html_static_path = ["_static"]

html_theme_options = {
    "navigation_depth": 3,
    "collapse_navigation": False,
}

myst_heading_anchors = 3
