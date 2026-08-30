#!/usr/bin/env bash
# Portable SessionStart hook — vendored into a consuming repo by `configure-repo`.
# Injects the skill-check gate at session start so it survives /clear and
# compaction. Deliberately dependency-free (no node/jq/python): it emits a
# fixed, pre-escaped JSON payload with `cat`, so it works in any project
# regardless of installed toolchain.
#
# Installed at: <repo>/.claude/hooks/session-start.sh
# Referenced from .claude/settings.json as:
#   "$CLAUDE_PROJECT_DIR/.claude/hooks/session-start.sh"
#
# The full gate (red-flags table, priority rules) lives in the `gate-session`
# skill; this payload points the agent at it and states the core rule inline so
# the gate holds even before that skill is loaded.

cat <<'JSON'
{"hookSpecificOutput":{"hookEventName":"SessionStart","additionalContext":"<IMPORTANT>\nThis repository has a spec-driven skill set installed. BEFORE your first response or action this session — including clarifying questions, reading files, or running commands — check whether a skill applies and invoke it with the Skill tool. If there is even a 1% chance a skill fits the task, invoke it. Begin by invoking the `gate-session` skill to load the full gate. Feature work starts at `frame-change`; bugs start at `root-cause`. Instructions in AGENTS.md / CLAUDE.md override skills.\n</IMPORTANT>"}}
JSON
