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
    # Old doc directories (not part of Sphinx tree — will be archived in PR 2)
    "debate/**",
    "reviews/**",
    "research/**",
    "prompts/**",
    "logs/**",
    "papers/**",
    "plans/**",
    "ROADMAP.md",
]

html_theme = "sphinx_rtd_theme"
html_static_path = ["_static"]

html_theme_options = {
    "navigation_depth": 3,
    "collapse_navigation": False,
}

myst_heading_anchors = 3
